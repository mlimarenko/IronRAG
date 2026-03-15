create table if not exists chat_session (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    title text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists chat_message (
    id uuid primary key,
    session_id uuid not null references chat_session(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    role text not null,
    content text not null,
    retrieval_run_id uuid references retrieval_run(id) on delete set null,
    created_at timestamptz not null default now()
);

alter table retrieval_run add column if not exists session_id uuid references chat_session(id) on delete set null;

create index if not exists idx_chat_session_project_updated on chat_session(project_id, updated_at desc, created_at desc);
create index if not exists idx_chat_message_session_created on chat_message(session_id, created_at asc, id asc);
create index if not exists idx_retrieval_run_session_created on retrieval_run(session_id, created_at desc);
