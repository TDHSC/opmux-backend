-- Private application schema for tenant clients and API key digests.
-- Not exposed through PostgREST/Data API. Apply via scripts/db-migrate.sh;
-- gateway replicas must not run this automatically.

CREATE SCHEMA opmux_private;
COMMENT ON SCHEMA opmux_private IS
  'Application-private persistence; not exposed through PostgREST/Data API.';

REVOKE ALL ON SCHEMA opmux_private FROM PUBLIC;
REVOKE ALL ON SCHEMA opmux_private FROM anon, authenticated, authenticator, service_role;

CREATE ROLE opmux_runtime NOLOGIN;
COMMENT ON ROLE opmux_runtime IS
  'Runtime gateway role: read clients; read/insert/update API keys.';

CREATE ROLE opmux_operator NOLOGIN;
COMMENT ON ROLE opmux_operator IS
  'Operator/provisioning role: DML on clients and API keys. Not a migrator.';

-- PostgreSQL 16+ requires SET TRUE for SET ROLE, even for the creating role.
GRANT opmux_runtime TO CURRENT_USER WITH INHERIT FALSE, SET TRUE;
GRANT opmux_operator TO CURRENT_USER WITH INHERIT FALSE, SET TRUE;

CREATE TABLE opmux_private.clients (
  id uuid PRIMARY KEY,
  display_name text NOT NULL,
  created_at timestamptz NOT NULL,
  CONSTRAINT clients_display_name_len CHECK (
    char_length(display_name) BETWEEN 1 AND 128
  )
);

COMMENT ON TABLE opmux_private.clients IS
  'One client is one tenant. No organizations, projects, or user profiles.';

CREATE TABLE opmux_private.api_keys (
  id uuid PRIMARY KEY,
  client_id uuid NOT NULL,
  key_digest bytea NOT NULL,
  display_id text NOT NULL,
  name text NOT NULL,
  kind text NOT NULL,
  created_at timestamptz NOT NULL,
  last_used_at timestamptz,
  revoked_at timestamptz,
  CONSTRAINT api_keys_client_fk
    FOREIGN KEY (client_id) REFERENCES opmux_private.clients (id)
    ON DELETE RESTRICT,
  CONSTRAINT api_keys_key_digest_key UNIQUE (key_digest),
  CONSTRAINT api_keys_display_id_key UNIQUE (display_id),
  CONSTRAINT api_keys_digest_len CHECK (octet_length(key_digest) = 32),
  CONSTRAINT api_keys_kind_known CHECK (kind IN ('management', 'inference')),
  CONSTRAINT api_keys_name_len CHECK (char_length(name) BETWEEN 1 AND 128),
  CONSTRAINT api_keys_display_id_len CHECK (
    char_length(display_id) BETWEEN 8 AND 64
  ),
  CONSTRAINT api_keys_display_id_format CHECK (display_id ~ '^[A-Za-z0-9_-]+$'),
  CONSTRAINT api_keys_last_used_order CHECK (
    last_used_at IS NULL OR last_used_at >= created_at
  ),
  CONSTRAINT api_keys_revoked_order CHECK (
    revoked_at IS NULL OR revoked_at >= created_at
  )
);

COMMENT ON TABLE opmux_private.api_keys IS
  'API keys stored as SHA-256 digests only; no plaintext credentials.';
COMMENT ON COLUMN opmux_private.api_keys.key_digest IS
  'SHA-256 digest (32 bytes) of the generated credential.';
COMMENT ON COLUMN opmux_private.api_keys.display_id IS
  'Safe public identifier. Not the secret and not the digest.';
COMMENT ON COLUMN opmux_private.api_keys.kind IS
  'Immutable management or inference kind.';

CREATE INDEX api_keys_client_inventory_idx
  ON opmux_private.api_keys (client_id, created_at DESC, id DESC);

CREATE FUNCTION opmux_private.reject_api_key_identity_mutation()
RETURNS trigger
LANGUAGE plpgsql
SET search_path TO opmux_private, pg_temp
AS $$
BEGIN
  IF NEW.id IS DISTINCT FROM OLD.id
     OR NEW.client_id IS DISTINCT FROM OLD.client_id
     OR NEW.key_digest IS DISTINCT FROM OLD.key_digest
     OR NEW.kind IS DISTINCT FROM OLD.kind
     OR NEW.display_id IS DISTINCT FROM OLD.display_id THEN
    RAISE EXCEPTION 'api key identity columns are immutable'
      USING ERRCODE = 'integrity_constraint_violation';
  END IF;
  RETURN NEW;
END;
$$;

CREATE TRIGGER api_keys_identity_immutable
  BEFORE UPDATE ON opmux_private.api_keys
  FOR EACH ROW
  EXECUTE FUNCTION opmux_private.reject_api_key_identity_mutation();

REVOKE ALL ON TABLE opmux_private.clients FROM PUBLIC;
REVOKE ALL ON TABLE opmux_private.clients FROM anon, authenticated, authenticator, service_role;
REVOKE ALL ON TABLE opmux_private.api_keys FROM PUBLIC;
REVOKE ALL ON TABLE opmux_private.api_keys FROM anon, authenticated, authenticator, service_role;

GRANT USAGE ON SCHEMA opmux_private TO opmux_runtime, opmux_operator;

GRANT SELECT ON TABLE opmux_private.clients TO opmux_runtime;
GRANT SELECT, INSERT, UPDATE ON TABLE opmux_private.api_keys TO opmux_runtime;

GRANT SELECT, INSERT, UPDATE ON TABLE opmux_private.clients TO opmux_operator;
GRANT SELECT, INSERT, UPDATE ON TABLE opmux_private.api_keys TO opmux_operator;
