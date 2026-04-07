package com.example.users

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.time.Instant
import java.util.UUID

/**
 * Represents a user account in the system.
 */
data class User(
    val id: UUID,
    val email: String,
    val displayName: String,
    val role: Role,
    val isActive: Boolean,
    val createdAt: Instant,
    val lastLoginAt: Instant?
) {
    enum class Role { VIEWER, EDITOR, ADMIN }
}

/**
 * Data class for creating a new user. Validates email format
 * and enforces display name length constraints.
 */
data class CreateUserRequest(
    val email: String,
    val displayName: String,
    val role: User.Role = User.Role.VIEWER
) {
    init {
        require(email.contains("@")) { "Invalid email format: $email" }
        require(displayName.length in 2..100) {
            "Display name must be between 2 and 100 characters"
        }
    }
}

/**
 * Repository for managing user persistence and retrieval.
 * All database operations are performed on the IO dispatcher.
 */
class UserRepository(private val database: Database) {

    /**
     * Finds a user by their unique identifier.
     * Returns null if no user exists with the given ID.
     *
     * @param id The UUID of the user to find.
     * @return The user, or null if not found.
     */
    suspend fun findById(id: UUID): User? = withContext(Dispatchers.IO) {
        database.query("SELECT * FROM users WHERE id = ?", id) { row ->
            User(
                id = row.getUUID("id"),
                email = row.getString("email"),
                displayName = row.getString("display_name"),
                role = User.Role.valueOf(row.getString("role")),
                isActive = row.getBoolean("is_active"),
                createdAt = row.getInstant("created_at"),
                lastLoginAt = row.getInstantOrNull("last_login_at")
            )
        }.firstOrNull()
    }

    /**
     * Searches for users matching the given query string.
     * Matches against email and display name using case-insensitive
     * partial matching. Results are limited to the specified count.
     *
     * @param query The search string.
     * @param limit Maximum number of results to return.
     * @return List of matching users.
     */
    suspend fun search(query: String, limit: Int = 20): List<User> =
        withContext(Dispatchers.IO) {
            val pattern = "%${query.lowercase()}%"
            database.query(
                """
                SELECT * FROM users
                WHERE LOWER(email) LIKE ? OR LOWER(display_name) LIKE ?
                ORDER BY display_name
                LIMIT ?
                """.trimIndent(),
                pattern, pattern, limit
            ) { row ->
                User(
                    id = row.getUUID("id"),
                    email = row.getString("email"),
                    displayName = row.getString("display_name"),
                    role = User.Role.valueOf(row.getString("role")),
                    isActive = row.getBoolean("is_active"),
                    createdAt = row.getInstant("created_at"),
                    lastLoginAt = row.getInstantOrNull("last_login_at")
                )
            }
        }

    /**
     * Creates a new user from the given request.
     * Assigns a new UUID and sets the created timestamp to now.
     * Throws if a user with the same email already exists.
     *
     * @param request The user creation parameters.
     * @return The newly created user.
     * @throws IllegalStateException if the email is already taken.
     */
    suspend fun create(request: CreateUserRequest): User = withContext(Dispatchers.IO) {
        val existing = database.query(
            "SELECT id FROM users WHERE email = ?", request.email
        ) { it }.firstOrNull()

        if (existing != null) {
            throw IllegalStateException("Email already registered: ${request.email}")
        }

        val user = User(
            id = UUID.randomUUID(),
            email = request.email,
            displayName = request.displayName,
            role = request.role,
            isActive = true,
            createdAt = Instant.now(),
            lastLoginAt = null
        )

        database.execute(
            """
            INSERT INTO users (id, email, display_name, role, is_active, created_at)
            VALUES (?, ?, ?, ?, ?, ?)
            """.trimIndent(),
            user.id, user.email, user.displayName,
            user.role.name, user.isActive, user.createdAt
        )

        user
    }
}
