-- Schema for a project management system.
-- Supports users, projects, tasks, and time tracking.

-- Users table: stores account information and authentication data.
-- Each user has a unique email and belongs to one organization.
CREATE TABLE users (
    id          BIGSERIAL PRIMARY KEY,
    email       VARCHAR(255) NOT NULL UNIQUE,
    full_name   VARCHAR(200) NOT NULL,
    org_id      BIGINT NOT NULL REFERENCES organizations(id),
    role        VARCHAR(50) NOT NULL DEFAULT 'member'
                CHECK (role IN ('admin', 'manager', 'member', 'viewer')),
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_users_org_id ON users(org_id);
CREATE INDEX idx_users_email_lower ON users(LOWER(email));

-- Projects table: represents a body of work with defined timelines.
-- A project belongs to an organization and has a designated owner.
CREATE TABLE projects (
    id           BIGSERIAL PRIMARY KEY,
    name         VARCHAR(200) NOT NULL,
    description  TEXT,
    org_id       BIGINT NOT NULL REFERENCES organizations(id),
    owner_id     BIGINT NOT NULL REFERENCES users(id),
    status       VARCHAR(30) NOT NULL DEFAULT 'planning'
                 CHECK (status IN ('planning', 'active', 'paused', 'completed', 'archived')),
    start_date   DATE,
    target_date  DATE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_projects_org_status ON projects(org_id, status);
CREATE INDEX idx_projects_owner ON projects(owner_id);

-- Tasks table: individual work items within a project.
-- Each task is assigned to a user and tracks effort estimates.
CREATE TABLE tasks (
    id              BIGSERIAL PRIMARY KEY,
    project_id      BIGINT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    title           VARCHAR(300) NOT NULL,
    description     TEXT,
    assignee_id     BIGINT REFERENCES users(id),
    status          VARCHAR(30) NOT NULL DEFAULT 'todo'
                    CHECK (status IN ('todo', 'in_progress', 'review', 'done', 'cancelled')),
    priority        SMALLINT NOT NULL DEFAULT 3
                    CHECK (priority BETWEEN 1 AND 5),
    estimated_hours NUMERIC(6,2),
    due_date        DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ
);

CREATE INDEX idx_tasks_project_status ON tasks(project_id, status);
CREATE INDEX idx_tasks_assignee ON tasks(assignee_id) WHERE assignee_id IS NOT NULL;
CREATE INDEX idx_tasks_due_date ON tasks(due_date) WHERE due_date IS NOT NULL AND status != 'done';

-- Time entries table: tracks actual hours spent on tasks.
-- Linked to both the task and the user who logged the time.
CREATE TABLE time_entries (
    id          BIGSERIAL PRIMARY KEY,
    task_id     BIGINT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users(id),
    hours       NUMERIC(5,2) NOT NULL CHECK (hours > 0),
    description VARCHAR(500),
    logged_date DATE NOT NULL DEFAULT CURRENT_DATE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_time_entries_task ON time_entries(task_id);
CREATE INDEX idx_time_entries_user_date ON time_entries(user_id, logged_date);

-- View: project progress summary showing task counts and completion percentage.
-- Aggregates task status data for dashboard reporting.
CREATE VIEW project_progress AS
SELECT
    p.id AS project_id,
    p.name AS project_name,
    p.status AS project_status,
    COUNT(t.id) AS total_tasks,
    COUNT(t.id) FILTER (WHERE t.status = 'done') AS completed_tasks,
    COUNT(t.id) FILTER (WHERE t.status = 'in_progress') AS active_tasks,
    ROUND(
        100.0 * COUNT(t.id) FILTER (WHERE t.status = 'done') /
        NULLIF(COUNT(t.id), 0),
        1
    ) AS completion_pct,
    COALESCE(SUM(t.estimated_hours), 0) AS total_estimated_hours,
    COALESCE(SUM(te.actual_hours), 0) AS total_actual_hours
FROM projects p
LEFT JOIN tasks t ON t.project_id = p.id
LEFT JOIN (
    SELECT task_id, SUM(hours) AS actual_hours
    FROM time_entries
    GROUP BY task_id
) te ON te.task_id = t.id
GROUP BY p.id, p.name, p.status;

-- View: user workload showing current task assignments and time logged this week.
-- Helps managers balance work distribution across team members.
CREATE VIEW user_workload AS
SELECT
    u.id AS user_id,
    u.full_name,
    COUNT(t.id) FILTER (WHERE t.status IN ('todo', 'in_progress', 'review')) AS open_tasks,
    COALESCE(SUM(t.estimated_hours) FILTER (WHERE t.status IN ('todo', 'in_progress')), 0) AS pending_hours,
    COALESCE(week_time.hours_this_week, 0) AS hours_this_week
FROM users u
LEFT JOIN tasks t ON t.assignee_id = u.id
LEFT JOIN (
    SELECT user_id, SUM(hours) AS hours_this_week
    FROM time_entries
    WHERE logged_date >= date_trunc('week', CURRENT_DATE)
    GROUP BY user_id
) week_time ON week_time.user_id = u.id
WHERE u.is_active = true
GROUP BY u.id, u.full_name, week_time.hours_this_week;

-- Function: automatically updates the updated_at timestamp on row modification.
CREATE OR REPLACE FUNCTION update_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_users_updated
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_timestamp();

CREATE TRIGGER trg_projects_updated
    BEFORE UPDATE ON projects
    FOR EACH ROW EXECUTE FUNCTION update_timestamp();
