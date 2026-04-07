-- Community detection results stored per node
ALTER TABLE runtime_graph_node ADD COLUMN IF NOT EXISTS community_id integer;
ALTER TABLE runtime_graph_node ADD COLUMN IF NOT EXISTS community_level integer DEFAULT 0;

-- Community metadata table
CREATE TABLE IF NOT EXISTS runtime_graph_community (
    id serial PRIMARY KEY,
    library_id uuid NOT NULL,
    community_id integer NOT NULL,
    level integer NOT NULL DEFAULT 0,
    node_count integer NOT NULL DEFAULT 0,
    edge_count integer NOT NULL DEFAULT 0,
    summary text,
    top_entities text[] NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (library_id, community_id, level)
);
CREATE INDEX IF NOT EXISTS idx_community_library ON runtime_graph_community(library_id);
