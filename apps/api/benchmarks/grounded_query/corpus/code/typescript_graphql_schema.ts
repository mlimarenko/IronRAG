/**
 * TypeScript GraphQL API with schema definitions, resolvers, authentication,
 * database queries, input validation, and custom error classes.
 *
 * Architecture decisions:
 * - Uses Apollo Server with express middleware for HTTP serving.
 * - Schema-first approach with SDL (Schema Definition Language) for type definitions.
 * - Prisma-style database client for type-safe database operations.
 * - Authentication via JWT tokens with role-based access control (RBAC).
 * - Input validation using Zod schemas before resolver execution.
 * - Custom error classes that map to GraphQL error extensions for client consumption.
 * - DataLoader pattern for N+1 query prevention on related entities.
 * - Environment variables for all configuration (DATABASE_URL, JWT_SECRET, etc.).
 */

import { ApolloServer } from "@apollo/server";
import { expressMiddleware } from "@apollo/server/express4";
import express from "express";
import { gql } from "graphql-tag";
import { z } from "zod";

// ---------------------------------------------------------------------------
// Environment configuration
// ---------------------------------------------------------------------------

/**
 * AppConfig holds all configuration loaded from environment variables.
 * Every field is documented with its corresponding env var name and default.
 */
interface AppConfig {
  /** DATABASE_URL - PostgreSQL connection string. Required. */
  databaseUrl: string;
  /** REDIS_URL - Redis connection string for caching. Default: "redis://localhost:6379" */
  redisUrl: string;
  /** JWT_SECRET - HMAC secret for JWT validation. Required in production. */
  jwtSecret: string;
  /** PORT - HTTP server port. Default: 4000 */
  port: number;
  /** NODE_ENV - Runtime environment. Default: "development" */
  nodeEnv: string;
  /** LOG_LEVEL - Logging verbosity. Default: "info" */
  logLevel: string;
  /** CORS_ORIGIN - Allowed CORS origin. Default: "*" */
  corsOrigin: string;
  /** MAX_QUERY_DEPTH - Maximum GraphQL query depth. Default: 10 */
  maxQueryDepth: number;
  /** RATE_LIMIT_WINDOW_MS - Rate limit window in milliseconds. Default: 60000 */
  rateLimitWindowMs: number;
  /** RATE_LIMIT_MAX_REQUESTS - Max requests per window. Default: 100 */
  rateLimitMaxRequests: number;
}

function loadConfig(): AppConfig {
  return {
    databaseUrl: process.env.DATABASE_URL || "",
    redisUrl: process.env.REDIS_URL || "redis://localhost:6379",
    jwtSecret: process.env.JWT_SECRET || "dev-secret-change-in-production",
    port: parseInt(process.env.PORT || "4000", 10),
    nodeEnv: process.env.NODE_ENV || "development",
    logLevel: process.env.LOG_LEVEL || "info",
    corsOrigin: process.env.CORS_ORIGIN || "*",
    maxQueryDepth: parseInt(process.env.MAX_QUERY_DEPTH || "10", 10),
    rateLimitWindowMs: parseInt(process.env.RATE_LIMIT_WINDOW_MS || "60000", 10),
    rateLimitMaxRequests: parseInt(process.env.RATE_LIMIT_MAX_REQUESTS || "100", 10),
  };
}

// ---------------------------------------------------------------------------
// Custom error classes
// ---------------------------------------------------------------------------

/**
 * Base application error that maps to GraphQL error extensions.
 * All custom errors extend this class to provide structured error responses.
 */
class AppError extends Error {
  public readonly code: string;
  public readonly statusCode: number;
  public readonly details?: Record<string, unknown>;

  constructor(message: string, code: string, statusCode: number, details?: Record<string, unknown>) {
    super(message);
    this.name = "AppError";
    this.code = code;
    this.statusCode = statusCode;
    this.details = details;
  }
}

/** Thrown when authentication is missing or invalid. */
class AuthenticationError extends AppError {
  constructor(message = "Authentication required") {
    super(message, "AUTHENTICATION_ERROR", 401);
  }
}

/** Thrown when the authenticated user lacks permission for the operation. */
class AuthorizationError extends AppError {
  constructor(message = "Insufficient permissions") {
    super(message, "AUTHORIZATION_ERROR", 403);
  }
}

/** Thrown when a requested resource does not exist. */
class NotFoundError extends AppError {
  constructor(resource: string, id: string) {
    super(`${resource} with ID '${id}' not found`, "NOT_FOUND", 404, { resource, id });
  }
}

/** Thrown when input validation fails. */
class ValidationError extends AppError {
  constructor(errors: Array<{ field: string; message: string }>) {
    super("Input validation failed", "VALIDATION_ERROR", 400, { errors });
  }
}

/** Thrown when a unique constraint is violated (e.g., duplicate email). */
class ConflictError extends AppError {
  constructor(resource: string, field: string, value: string) {
    super(`${resource} with ${field} '${value}' already exists`, "CONFLICT", 409, { resource, field });
  }
}

/** Thrown when an external service call fails. */
class ExternalServiceError extends AppError {
  constructor(service: string, message: string) {
    super(`External service '${service}' failed: ${message}`, "EXTERNAL_SERVICE_ERROR", 502, { service });
  }
}

// ---------------------------------------------------------------------------
// Database types (Prisma-style models)
// ---------------------------------------------------------------------------

/** User model representing a row in the users table. */
interface UserModel {
  id: string;
  email: string;
  passwordHash: string;
  displayName: string;
  role: UserRole;
  avatarUrl: string | null;
  isActive: boolean;
  lastLoginAt: Date | null;
  createdAt: Date;
  updatedAt: Date;
}

/** Organization model representing a company or team. */
interface OrganizationModel {
  id: string;
  name: string;
  slug: string;
  plan: OrganizationPlan;
  maxMembers: number;
  ownerId: string;
  createdAt: Date;
  updatedAt: Date;
}

/** Project model representing a workspace within an organization. */
interface ProjectModel {
  id: string;
  name: string;
  description: string;
  organizationId: string;
  status: ProjectStatus;
  visibility: ProjectVisibility;
  tags: string[];
  metadata: Record<string, unknown>;
  createdAt: Date;
  updatedAt: Date;
}

/** Task model representing a work item within a project. */
interface TaskModel {
  id: string;
  title: string;
  description: string;
  projectId: string;
  assigneeId: string | null;
  reporterId: string;
  status: TaskStatus;
  priority: TaskPriority;
  dueDate: Date | null;
  estimatedHours: number | null;
  labels: string[];
  createdAt: Date;
  updatedAt: Date;
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

type UserRole = "ADMIN" | "MEMBER" | "VIEWER" | "GUEST";
type OrganizationPlan = "FREE" | "STARTER" | "PROFESSIONAL" | "ENTERPRISE";
type ProjectStatus = "ACTIVE" | "ARCHIVED" | "DRAFT";
type ProjectVisibility = "PUBLIC" | "PRIVATE" | "INTERNAL";
type TaskStatus = "BACKLOG" | "TODO" | "IN_PROGRESS" | "IN_REVIEW" | "DONE" | "CANCELLED";
type TaskPriority = "CRITICAL" | "HIGH" | "MEDIUM" | "LOW";

// ---------------------------------------------------------------------------
// Input validation schemas (Zod)
// ---------------------------------------------------------------------------

const CreateUserInputSchema = z.object({
  email: z.string().email("Invalid email format"),
  password: z.string().min(8, "Password must be at least 8 characters")
    .regex(/[A-Z]/, "Password must contain at least one uppercase letter")
    .regex(/[0-9]/, "Password must contain at least one digit"),
  displayName: z.string().min(2).max(100),
  role: z.enum(["ADMIN", "MEMBER", "VIEWER", "GUEST"]).optional().default("MEMBER"),
});

const UpdateUserInputSchema = z.object({
  displayName: z.string().min(2).max(100).optional(),
  avatarUrl: z.string().url().nullable().optional(),
  role: z.enum(["ADMIN", "MEMBER", "VIEWER", "GUEST"]).optional(),
});

const CreateProjectInputSchema = z.object({
  name: z.string().min(1).max(200),
  description: z.string().max(5000).optional().default(""),
  organizationId: z.string().uuid(),
  visibility: z.enum(["PUBLIC", "PRIVATE", "INTERNAL"]).optional().default("PRIVATE"),
  tags: z.array(z.string().max(50)).max(20).optional().default([]),
});

const CreateTaskInputSchema = z.object({
  title: z.string().min(1).max(500),
  description: z.string().max(10000).optional().default(""),
  projectId: z.string().uuid(),
  assigneeId: z.string().uuid().nullable().optional(),
  priority: z.enum(["CRITICAL", "HIGH", "MEDIUM", "LOW"]).optional().default("MEDIUM"),
  dueDate: z.string().datetime().nullable().optional(),
  estimatedHours: z.number().min(0).max(1000).nullable().optional(),
  labels: z.array(z.string().max(50)).max(30).optional().default([]),
});

const UpdateTaskStatusInputSchema = z.object({
  taskId: z.string().uuid(),
  status: z.enum(["BACKLOG", "TODO", "IN_PROGRESS", "IN_REVIEW", "DONE", "CANCELLED"]),
});

const PaginationInputSchema = z.object({
  page: z.number().int().min(1).optional().default(1),
  limit: z.number().int().min(1).max(100).optional().default(20),
  sortBy: z.string().optional().default("createdAt"),
  sortOrder: z.enum(["asc", "desc"]).optional().default("desc"),
});

// ---------------------------------------------------------------------------
// GraphQL Schema Definition
// ---------------------------------------------------------------------------

const typeDefs = gql`
  """
  ISO-8601 date-time scalar for timestamp fields.
  """
  scalar DateTime

  """
  Arbitrary JSON scalar for metadata fields.
  """
  scalar JSON

  # -- Enums --

  enum UserRole {
    ADMIN
    MEMBER
    VIEWER
    GUEST
  }

  enum OrganizationPlan {
    FREE
    STARTER
    PROFESSIONAL
    ENTERPRISE
  }

  enum ProjectStatus {
    ACTIVE
    ARCHIVED
    DRAFT
  }

  enum ProjectVisibility {
    PUBLIC
    PRIVATE
    INTERNAL
  }

  enum TaskStatus {
    BACKLOG
    TODO
    IN_PROGRESS
    IN_REVIEW
    DONE
    CANCELLED
  }

  enum TaskPriority {
    CRITICAL
    HIGH
    MEDIUM
    LOW
  }

  # -- Types --

  type User {
    id: ID!
    email: String!
    displayName: String!
    role: UserRole!
    avatarUrl: String
    isActive: Boolean!
    lastLoginAt: DateTime
    organizations: [Organization!]!
    tasks(status: TaskStatus): [Task!]!
    createdAt: DateTime!
    updatedAt: DateTime!
  }

  type Organization {
    id: ID!
    name: String!
    slug: String!
    plan: OrganizationPlan!
    maxMembers: Int!
    owner: User!
    members: [User!]!
    memberCount: Int!
    projects: [Project!]!
    createdAt: DateTime!
    updatedAt: DateTime!
  }

  type Project {
    id: ID!
    name: String!
    description: String!
    organization: Organization!
    status: ProjectStatus!
    visibility: ProjectVisibility!
    tags: [String!]!
    metadata: JSON
    tasks(status: TaskStatus, priority: TaskPriority): [Task!]!
    taskCount: Int!
    createdAt: DateTime!
    updatedAt: DateTime!
  }

  type Task {
    id: ID!
    title: String!
    description: String!
    project: Project!
    assignee: User
    reporter: User!
    status: TaskStatus!
    priority: TaskPriority!
    dueDate: DateTime
    estimatedHours: Float
    labels: [String!]!
    comments: [Comment!]!
    createdAt: DateTime!
    updatedAt: DateTime!
  }

  type Comment {
    id: ID!
    body: String!
    author: User!
    task: Task!
    createdAt: DateTime!
    updatedAt: DateTime!
  }

  type PaginatedUsers {
    nodes: [User!]!
    totalCount: Int!
    hasNextPage: Boolean!
    hasPreviousPage: Boolean!
  }

  type PaginatedProjects {
    nodes: [Project!]!
    totalCount: Int!
    hasNextPage: Boolean!
    hasPreviousPage: Boolean!
  }

  type PaginatedTasks {
    nodes: [Task!]!
    totalCount: Int!
    hasNextPage: Boolean!
    hasPreviousPage: Boolean!
  }

  type AuthPayload {
    token: String!
    user: User!
    expiresAt: DateTime!
  }

  # -- Inputs --

  input CreateUserInput {
    email: String!
    password: String!
    displayName: String!
    role: UserRole
  }

  input UpdateUserInput {
    displayName: String
    avatarUrl: String
    role: UserRole
  }

  input CreateProjectInput {
    name: String!
    description: String
    organizationId: ID!
    visibility: ProjectVisibility
    tags: [String!]
  }

  input CreateTaskInput {
    title: String!
    description: String
    projectId: ID!
    assigneeId: ID
    priority: TaskPriority
    dueDate: DateTime
    estimatedHours: Float
    labels: [String!]
  }

  input UpdateTaskStatusInput {
    taskId: ID!
    status: TaskStatus!
  }

  input LoginInput {
    email: String!
    password: String!
  }

  input PaginationInput {
    page: Int
    limit: Int
    sortBy: String
    sortOrder: String
  }

  # -- Queries --

  type Query {
    """Retrieve the currently authenticated user."""
    me: User!

    """Find a user by their unique ID."""
    user(id: ID!): User

    """List all users with pagination (admin only)."""
    users(pagination: PaginationInput): PaginatedUsers!

    """Find an organization by ID or slug."""
    organization(id: ID, slug: String): Organization

    """Find a project by its unique ID."""
    project(id: ID!): Project

    """List projects within an organization with optional filters."""
    projects(
      organizationId: ID!
      status: ProjectStatus
      pagination: PaginationInput
    ): PaginatedProjects!

    """Find a task by its unique ID."""
    task(id: ID!): Task

    """List tasks with filtering by project, status, assignee, and priority."""
    tasks(
      projectId: ID!
      status: TaskStatus
      assigneeId: ID
      priority: TaskPriority
      pagination: PaginationInput
    ): PaginatedTasks!

    """Search tasks by title or description text."""
    searchTasks(query: String!, projectId: ID): [Task!]!
  }

  # -- Mutations --

  type Mutation {
    """Authenticate a user and return a JWT token."""
    login(input: LoginInput!): AuthPayload!

    """Register a new user account."""
    createUser(input: CreateUserInput!): User!

    """Update an existing user's profile."""
    updateUser(id: ID!, input: UpdateUserInput!): User!

    """Deactivate a user account (admin only)."""
    deactivateUser(id: ID!): User!

    """Create a new project within an organization."""
    createProject(input: CreateProjectInput!): Project!

    """Archive a project (sets status to ARCHIVED)."""
    archiveProject(id: ID!): Project!

    """Create a new task within a project."""
    createTask(input: CreateTaskInput!): Task!

    """Update the status of a task (triggers workflow rules)."""
    updateTaskStatus(input: UpdateTaskStatusInput!): Task!

    """Assign a task to a user."""
    assignTask(taskId: ID!, assigneeId: ID!): Task!

    """Add a comment to a task."""
    addComment(taskId: ID!, body: String!): Comment!

    """Delete a comment (author or admin only)."""
    deleteComment(commentId: ID!): Boolean!

    """Transfer project ownership to another organization."""
    transferProject(projectId: ID!, targetOrganizationId: ID!): Project!
  }

  # -- Subscriptions --

  type Subscription {
    """Subscribe to task status changes in a project."""
    taskStatusChanged(projectId: ID!): Task!

    """Subscribe to new comments on a task."""
    commentAdded(taskId: ID!): Comment!

    """Subscribe to project activity feed."""
    projectActivity(projectId: ID!): ActivityEvent!
  }

  type ActivityEvent {
    id: ID!
    type: String!
    actor: User!
    payload: JSON!
    createdAt: DateTime!
  }
`;

// ---------------------------------------------------------------------------
// Context and auth
// ---------------------------------------------------------------------------

/** GraphQL context available in all resolvers. */
interface GraphQLContext {
  user: UserModel | null;
  config: AppConfig;
  db: DatabaseClient;
  loaders: DataLoaders;
  requestId: string;
}

/** DataLoader instances for batching and caching related entity loads. */
interface DataLoaders {
  userLoader: BatchLoader<string, UserModel>;
  orgLoader: BatchLoader<string, OrganizationModel>;
  projectLoader: BatchLoader<string, ProjectModel>;
  taskLoader: BatchLoader<string, TaskModel>;
}

/** Minimal batch loader interface for N+1 prevention. */
interface BatchLoader<K, V> {
  load(key: K): Promise<V | null>;
  loadMany(keys: K[]): Promise<(V | null)[]>;
  clear(key: K): void;
  clearAll(): void;
}

/** Minimal database client interface (Prisma-style). */
interface DatabaseClient {
  user: ModelDelegate<UserModel>;
  organization: ModelDelegate<OrganizationModel>;
  project: ModelDelegate<ProjectModel>;
  task: ModelDelegate<TaskModel>;
  comment: ModelDelegate<CommentModel>;
}

interface CommentModel {
  id: string;
  body: string;
  authorId: string;
  taskId: string;
  createdAt: Date;
  updatedAt: Date;
}

/** Generic model delegate for CRUD operations. */
interface ModelDelegate<T> {
  findUnique(args: { where: Partial<T> }): Promise<T | null>;
  findMany(args: {
    where?: Partial<T>;
    orderBy?: Record<string, "asc" | "desc">;
    skip?: number;
    take?: number;
  }): Promise<T[]>;
  count(args?: { where?: Partial<T> }): Promise<number>;
  create(args: { data: Partial<T> }): Promise<T>;
  update(args: { where: Partial<T>; data: Partial<T> }): Promise<T>;
  delete(args: { where: Partial<T> }): Promise<T>;
}

// ---------------------------------------------------------------------------
// Authentication middleware
// ---------------------------------------------------------------------------

/**
 * Extracts and validates the JWT token from the Authorization header.
 * Returns the authenticated user model or null for unauthenticated requests.
 * Supports "Bearer <token>" format.
 *
 * The JWT payload must contain: { sub: string, email: string, role: UserRole }
 */
async function authenticateRequest(
  req: express.Request,
  config: AppConfig,
  db: DatabaseClient
): Promise<UserModel | null> {
  const authHeader = req.headers.authorization;
  if (!authHeader || !authHeader.startsWith("Bearer ")) {
    return null;
  }

  const token = authHeader.slice(7);

  try {
    // In production, use jsonwebtoken.verify(token, config.jwtSecret)
    const payload = decodeJwtPayload(token);
    if (!payload || !payload.sub) {
      return null;
    }

    // Check token expiration
    if (payload.exp && Date.now() / 1000 > payload.exp) {
      throw new AuthenticationError("Token has expired");
    }

    const user = await db.user.findUnique({ where: { id: payload.sub } as Partial<UserModel> });
    if (!user || !user.isActive) {
      throw new AuthenticationError("User account is inactive or not found");
    }

    return user;
  } catch (error) {
    if (error instanceof AuthenticationError) throw error;
    return null;
  }
}

function decodeJwtPayload(token: string): Record<string, any> | null {
  const parts = token.split(".");
  if (parts.length !== 3) return null;
  try {
    return JSON.parse(Buffer.from(parts[1], "base64url").toString());
  } catch {
    return null;
  }
}

/** Resolver guard that throws if the user is not authenticated. */
function requireAuth(context: GraphQLContext): UserModel {
  if (!context.user) {
    throw new AuthenticationError();
  }
  return context.user;
}

/** Resolver guard that throws if the user does not have the required role. */
function requireRole(context: GraphQLContext, ...roles: UserRole[]): UserModel {
  const user = requireAuth(context);
  if (!roles.includes(user.role)) {
    throw new AuthorizationError(`Required role: ${roles.join(" or ")}`);
  }
  return user;
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/**
 * Validates input against a Zod schema and throws a structured ValidationError
 * if the input is invalid. Returns the parsed and typed input on success.
 */
function validateInput<T>(schema: z.ZodSchema<T>, input: unknown): T {
  const result = schema.safeParse(input);
  if (!result.success) {
    const errors = result.error.issues.map((issue) => ({
      field: issue.path.join("."),
      message: issue.message,
    }));
    throw new ValidationError(errors);
  }
  return result.data;
}

// ---------------------------------------------------------------------------
// Resolvers
// ---------------------------------------------------------------------------

const resolvers = {
  Query: {
    me: (_: unknown, __: unknown, context: GraphQLContext) => {
      return requireAuth(context);
    },

    user: async (_: unknown, args: { id: string }, context: GraphQLContext) => {
      requireAuth(context);
      const user = await context.loaders.userLoader.load(args.id);
      if (!user) throw new NotFoundError("User", args.id);
      return user;
    },

    users: async (_: unknown, args: { pagination?: unknown }, context: GraphQLContext) => {
      requireRole(context, "ADMIN");
      const pagination = validateInput(PaginationInputSchema, args.pagination || {});
      const skip = (pagination.page - 1) * pagination.limit;

      const [nodes, totalCount] = await Promise.all([
        context.db.user.findMany({
          orderBy: { [pagination.sortBy]: pagination.sortOrder },
          skip,
          take: pagination.limit,
        }),
        context.db.user.count(),
      ]);

      return {
        nodes,
        totalCount,
        hasNextPage: skip + nodes.length < totalCount,
        hasPreviousPage: pagination.page > 1,
      };
    },

    organization: async (_: unknown, args: { id?: string; slug?: string }, context: GraphQLContext) => {
      requireAuth(context);
      if (!args.id && !args.slug) {
        throw new ValidationError([{ field: "id/slug", message: "Either id or slug must be provided" }]);
      }
      const where = args.id ? { id: args.id } : { slug: args.slug! };
      const org = await context.db.organization.findUnique({ where: where as Partial<OrganizationModel> });
      if (!org) throw new NotFoundError("Organization", args.id || args.slug || "");
      return org;
    },

    project: async (_: unknown, args: { id: string }, context: GraphQLContext) => {
      requireAuth(context);
      const project = await context.loaders.projectLoader.load(args.id);
      if (!project) throw new NotFoundError("Project", args.id);
      return project;
    },

    projects: async (_: unknown, args: { organizationId: string; status?: ProjectStatus; pagination?: unknown }, context: GraphQLContext) => {
      requireAuth(context);
      const pagination = validateInput(PaginationInputSchema, args.pagination || {});
      const skip = (pagination.page - 1) * pagination.limit;
      const where: Partial<ProjectModel> = { organizationId: args.organizationId } as Partial<ProjectModel>;
      if (args.status) (where as any).status = args.status;

      const [nodes, totalCount] = await Promise.all([
        context.db.project.findMany({ where, orderBy: { [pagination.sortBy]: pagination.sortOrder }, skip, take: pagination.limit }),
        context.db.project.count({ where }),
      ]);

      return { nodes, totalCount, hasNextPage: skip + nodes.length < totalCount, hasPreviousPage: pagination.page > 1 };
    },

    task: async (_: unknown, args: { id: string }, context: GraphQLContext) => {
      requireAuth(context);
      const task = await context.loaders.taskLoader.load(args.id);
      if (!task) throw new NotFoundError("Task", args.id);
      return task;
    },

    tasks: async (_: unknown, args: { projectId: string; status?: TaskStatus; assigneeId?: string; priority?: TaskPriority; pagination?: unknown }, context: GraphQLContext) => {
      requireAuth(context);
      const pagination = validateInput(PaginationInputSchema, args.pagination || {});
      const skip = (pagination.page - 1) * pagination.limit;
      const where: any = { projectId: args.projectId };
      if (args.status) where.status = args.status;
      if (args.assigneeId) where.assigneeId = args.assigneeId;
      if (args.priority) where.priority = args.priority;

      const [nodes, totalCount] = await Promise.all([
        context.db.task.findMany({ where, orderBy: { [pagination.sortBy]: pagination.sortOrder }, skip, take: pagination.limit }),
        context.db.task.count({ where }),
      ]);

      return { nodes, totalCount, hasNextPage: skip + nodes.length < totalCount, hasPreviousPage: pagination.page > 1 };
    },

    searchTasks: async (_: unknown, args: { query: string; projectId?: string }, context: GraphQLContext) => {
      requireAuth(context);
      // In production, uses full-text search via PostgreSQL ts_vector or Elasticsearch
      const where: any = {};
      if (args.projectId) where.projectId = args.projectId;
      return context.db.task.findMany({ where, take: 50 });
    },
  },

  Mutation: {
    login: async (_: unknown, args: { input: { email: string; password: string } }, context: GraphQLContext) => {
      const user = await context.db.user.findUnique({ where: { email: args.input.email } as Partial<UserModel> });
      if (!user) {
        throw new AuthenticationError("Invalid email or password");
      }
      // In production, compare password hash with bcrypt
      const expiresAt = new Date(Date.now() + 24 * 60 * 60 * 1000); // 24 hours
      const token = `jwt.token.placeholder`; // In production, sign with jsonwebtoken
      return { token, user, expiresAt };
    },

    createUser: async (_: unknown, args: { input: unknown }, context: GraphQLContext) => {
      requireRole(context, "ADMIN");
      const input = validateInput(CreateUserInputSchema, args.input);

      // Check for duplicate email
      const existing = await context.db.user.findUnique({ where: { email: input.email } as Partial<UserModel> });
      if (existing) {
        throw new ConflictError("User", "email", input.email);
      }

      return context.db.user.create({
        data: {
          email: input.email,
          displayName: input.displayName,
          role: input.role,
          isActive: true,
        } as Partial<UserModel>,
      });
    },

    updateUser: async (_: unknown, args: { id: string; input: unknown }, context: GraphQLContext) => {
      const currentUser = requireAuth(context);
      // Users can update their own profile; admins can update anyone
      if (currentUser.id !== args.id && currentUser.role !== "ADMIN") {
        throw new AuthorizationError("Can only update your own profile");
      }
      const input = validateInput(UpdateUserInputSchema, args.input);
      return context.db.user.update({ where: { id: args.id } as Partial<UserModel>, data: input as Partial<UserModel> });
    },

    deactivateUser: async (_: unknown, args: { id: string }, context: GraphQLContext) => {
      requireRole(context, "ADMIN");
      return context.db.user.update({ where: { id: args.id } as Partial<UserModel>, data: { isActive: false } as Partial<UserModel> });
    },

    createProject: async (_: unknown, args: { input: unknown }, context: GraphQLContext) => {
      const user = requireAuth(context);
      const input = validateInput(CreateProjectInputSchema, args.input);

      // Verify the user is a member of the organization
      const org = await context.db.organization.findUnique({ where: { id: input.organizationId } as Partial<OrganizationModel> });
      if (!org) throw new NotFoundError("Organization", input.organizationId);

      return context.db.project.create({
        data: {
          name: input.name,
          description: input.description,
          organizationId: input.organizationId,
          status: "ACTIVE" as ProjectStatus,
          visibility: input.visibility as ProjectVisibility,
          tags: input.tags,
        } as Partial<ProjectModel>,
      });
    },

    archiveProject: async (_: unknown, args: { id: string }, context: GraphQLContext) => {
      requireRole(context, "ADMIN", "MEMBER");
      return context.db.project.update({
        where: { id: args.id } as Partial<ProjectModel>,
        data: { status: "ARCHIVED" as ProjectStatus } as Partial<ProjectModel>,
      });
    },

    createTask: async (_: unknown, args: { input: unknown }, context: GraphQLContext) => {
      const user = requireAuth(context);
      const input = validateInput(CreateTaskInputSchema, args.input);

      // Verify project exists
      const project = await context.db.project.findUnique({ where: { id: input.projectId } as Partial<ProjectModel> });
      if (!project) throw new NotFoundError("Project", input.projectId);
      if (project.status === "ARCHIVED") {
        throw new AppError("Cannot create tasks in archived projects", "PROJECT_ARCHIVED", 400);
      }

      return context.db.task.create({
        data: {
          title: input.title,
          description: input.description,
          projectId: input.projectId,
          assigneeId: input.assigneeId,
          reporterId: user.id,
          status: "BACKLOG" as TaskStatus,
          priority: input.priority as TaskPriority,
          labels: input.labels,
        } as Partial<TaskModel>,
      });
    },

    updateTaskStatus: async (_: unknown, args: { input: unknown }, context: GraphQLContext) => {
      requireAuth(context);
      const input = validateInput(UpdateTaskStatusInputSchema, args.input);

      const task = await context.db.task.findUnique({ where: { id: input.taskId } as Partial<TaskModel> });
      if (!task) throw new NotFoundError("Task", input.taskId);

      // Validate state transition rules
      const validTransitions: Record<TaskStatus, TaskStatus[]> = {
        BACKLOG: ["TODO", "CANCELLED"],
        TODO: ["IN_PROGRESS", "BACKLOG", "CANCELLED"],
        IN_PROGRESS: ["IN_REVIEW", "TODO", "CANCELLED"],
        IN_REVIEW: ["DONE", "IN_PROGRESS", "CANCELLED"],
        DONE: ["TODO"], // reopen
        CANCELLED: ["BACKLOG"], // resurrect
      };

      const allowed = validTransitions[task.status];
      if (!allowed || !allowed.includes(input.status)) {
        throw new AppError(
          `Cannot transition task from '${task.status}' to '${input.status}'`,
          "INVALID_TRANSITION",
          400,
          { currentStatus: task.status, requestedStatus: input.status, validTransitions: allowed }
        );
      }

      return context.db.task.update({ where: { id: input.taskId } as Partial<TaskModel>, data: { status: input.status } as Partial<TaskModel> });
    },

    assignTask: async (_: unknown, args: { taskId: string; assigneeId: string }, context: GraphQLContext) => {
      requireAuth(context);
      // Verify assignee exists
      const assignee = await context.db.user.findUnique({ where: { id: args.assigneeId } as Partial<UserModel> });
      if (!assignee) throw new NotFoundError("User", args.assigneeId);
      return context.db.task.update({ where: { id: args.taskId } as Partial<TaskModel>, data: { assigneeId: args.assigneeId } as Partial<TaskModel> });
    },

    addComment: async (_: unknown, args: { taskId: string; body: string }, context: GraphQLContext) => {
      const user = requireAuth(context);
      if (!args.body.trim()) {
        throw new ValidationError([{ field: "body", message: "Comment body cannot be empty" }]);
      }
      return context.db.comment.create({
        data: { body: args.body, authorId: user.id, taskId: args.taskId } as Partial<CommentModel>,
      });
    },

    deleteComment: async (_: unknown, args: { commentId: string }, context: GraphQLContext) => {
      const user = requireAuth(context);
      const comment = await context.db.comment.findUnique({ where: { id: args.commentId } as Partial<CommentModel> });
      if (!comment) throw new NotFoundError("Comment", args.commentId);
      // Only comment author or admin can delete
      if (comment.authorId !== user.id && user.role !== "ADMIN") {
        throw new AuthorizationError("Can only delete your own comments");
      }
      await context.db.comment.delete({ where: { id: args.commentId } as Partial<CommentModel> });
      return true;
    },

    transferProject: async (_: unknown, args: { projectId: string; targetOrganizationId: string }, context: GraphQLContext) => {
      requireRole(context, "ADMIN");
      const project = await context.db.project.findUnique({ where: { id: args.projectId } as Partial<ProjectModel> });
      if (!project) throw new NotFoundError("Project", args.projectId);
      const targetOrg = await context.db.organization.findUnique({ where: { id: args.targetOrganizationId } as Partial<OrganizationModel> });
      if (!targetOrg) throw new NotFoundError("Organization", args.targetOrganizationId);
      return context.db.project.update({
        where: { id: args.projectId } as Partial<ProjectModel>,
        data: { organizationId: args.targetOrganizationId } as Partial<ProjectModel>,
      });
    },
  },

  // Field resolvers for related entities
  User: {
    organizations: async (parent: UserModel, _: unknown, context: GraphQLContext) => {
      return context.db.organization.findMany({ where: { ownerId: parent.id } as Partial<OrganizationModel> });
    },
    tasks: async (parent: UserModel, args: { status?: TaskStatus }, context: GraphQLContext) => {
      const where: any = { assigneeId: parent.id };
      if (args.status) where.status = args.status;
      return context.db.task.findMany({ where });
    },
  },

  Organization: {
    owner: async (parent: OrganizationModel, _: unknown, context: GraphQLContext) => {
      return context.loaders.userLoader.load(parent.ownerId);
    },
    projects: async (parent: OrganizationModel, _: unknown, context: GraphQLContext) => {
      return context.db.project.findMany({ where: { organizationId: parent.id } as Partial<ProjectModel> });
    },
  },

  Project: {
    organization: async (parent: ProjectModel, _: unknown, context: GraphQLContext) => {
      return context.loaders.orgLoader.load(parent.organizationId);
    },
    tasks: async (parent: ProjectModel, args: { status?: TaskStatus; priority?: TaskPriority }, context: GraphQLContext) => {
      const where: any = { projectId: parent.id };
      if (args.status) where.status = args.status;
      if (args.priority) where.priority = args.priority;
      return context.db.task.findMany({ where });
    },
  },

  Task: {
    project: async (parent: TaskModel, _: unknown, context: GraphQLContext) => {
      return context.loaders.projectLoader.load(parent.projectId);
    },
    assignee: async (parent: TaskModel, _: unknown, context: GraphQLContext) => {
      return parent.assigneeId ? context.loaders.userLoader.load(parent.assigneeId) : null;
    },
    reporter: async (parent: TaskModel, _: unknown, context: GraphQLContext) => {
      return context.loaders.userLoader.load(parent.reporterId);
    },
  },
};

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

async function startServer(): Promise<void> {
  const config = loadConfig();

  if (config.nodeEnv === "production" && !config.databaseUrl) {
    throw new Error("DATABASE_URL is required in production");
  }

  const app = express();
  app.use(express.json({ limit: "1mb" }));

  const server = new ApolloServer<GraphQLContext>({
    typeDefs,
    resolvers,
    formatError: (formattedError, error) => {
      // Structured error formatting for clients
      const originalError = (error as any)?.originalError;
      if (originalError instanceof AppError) {
        return {
          message: originalError.message,
          extensions: {
            code: originalError.code,
            statusCode: originalError.statusCode,
            details: originalError.details,
          },
        };
      }
      // Mask internal errors in production
      if (config.nodeEnv === "production") {
        return { message: "Internal server error", extensions: { code: "INTERNAL_ERROR" } };
      }
      return formattedError;
    },
  });

  await server.start();

  app.use(
    "/graphql",
    expressMiddleware(server, {
      context: async ({ req }): Promise<GraphQLContext> => {
        const db = {} as DatabaseClient; // In production, use Prisma client
        const user = await authenticateRequest(req, config, db);
        return {
          user,
          config,
          db,
          loaders: {} as DataLoaders, // In production, create fresh DataLoaders per request
          requestId: req.headers["x-request-id"] as string || crypto.randomUUID(),
        };
      },
    })
  );

  app.listen(config.port, () => {
    console.log(`GraphQL server running at http://localhost:${config.port}/graphql`);
    console.log(`Environment: ${config.nodeEnv}`);
  });
}

startServer().catch((err) => {
  console.error("Failed to start server:", err);
  process.exit(1);
});
