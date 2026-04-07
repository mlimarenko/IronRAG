import Foundation
import SwiftUI
import Combine

/// Represents a task in the task management application.
struct TaskItem: Identifiable, Codable {
    let id: UUID
    var title: String
    var description: String
    var isCompleted: Bool
    var priority: Priority
    var dueDate: Date?
    var createdAt: Date

    enum Priority: String, Codable, CaseIterable {
        case low, medium, high, urgent
    }
}

/// ViewModel responsible for managing the task list state.
/// Handles loading, filtering, creating, and completing tasks.
/// Publishes changes to drive SwiftUI view updates.
@MainActor
class TaskListViewModel: ObservableObject {

    @Published var tasks: [TaskItem] = []
    @Published var filterPriority: TaskItem.Priority? = nil
    @Published var searchText: String = ""
    @Published var isLoading: Bool = false
    @Published var errorMessage: String? = nil

    private let taskService: TaskServiceProtocol
    private var cancellables = Set<AnyCancellable>()

    /// The filtered and sorted list of tasks based on current search
    /// text and priority filter. Tasks are sorted by priority descending,
    /// then by due date ascending.
    var filteredTasks: [TaskItem] {
        var result = tasks

        if let priority = filterPriority {
            result = result.filter { $0.priority == priority }
        }

        if !searchText.isEmpty {
            let query = searchText.lowercased()
            result = result.filter {
                $0.title.lowercased().contains(query) ||
                $0.description.lowercased().contains(query)
            }
        }

        return result.sorted { a, b in
            let priorities: [TaskItem.Priority] = [.urgent, .high, .medium, .low]
            let aPri = priorities.firstIndex(of: a.priority) ?? 3
            let bPri = priorities.firstIndex(of: b.priority) ?? 3
            if aPri != bPri { return aPri < bPri }
            guard let aDate = a.dueDate, let bDate = b.dueDate else {
                return a.dueDate != nil
            }
            return aDate < bDate
        }
    }

    init(taskService: TaskServiceProtocol) {
        self.taskService = taskService
    }

    /// Fetches all tasks from the backend service.
    /// Sets isLoading during the request and populates errorMessage on failure.
    func loadTasks() async {
        isLoading = true
        errorMessage = nil

        do {
            tasks = try await taskService.fetchAllTasks()
        } catch {
            errorMessage = "Failed to load tasks: \(error.localizedDescription)"
        }

        isLoading = false
    }

    /// Toggles the completion status of a task and persists the change.
    /// Optimistically updates the local state before confirming with the server.
    ///
    /// - Parameter taskId: The unique identifier of the task to toggle.
    func toggleCompletion(for taskId: UUID) async {
        guard let index = tasks.firstIndex(where: { $0.id == taskId }) else { return }

        tasks[index].isCompleted.toggle()

        do {
            try await taskService.updateTask(tasks[index])
        } catch {
            tasks[index].isCompleted.toggle()
            errorMessage = "Failed to update task: \(error.localizedDescription)"
        }
    }

    /// Creates a new task with the given parameters and appends it to the list.
    /// The task is assigned a new UUID and the current timestamp.
    ///
    /// - Parameters:
    ///   - title: The task title.
    ///   - description: A longer description of the task.
    ///   - priority: The task priority level.
    ///   - dueDate: Optional due date for the task.
    func createTask(title: String, description: String,
                    priority: TaskItem.Priority, dueDate: Date?) async {
        let newTask = TaskItem(
            id: UUID(),
            title: title,
            description: description,
            isCompleted: false,
            priority: priority,
            dueDate: dueDate,
            createdAt: Date()
        )

        do {
            let saved = try await taskService.createTask(newTask)
            tasks.append(saved)
        } catch {
            errorMessage = "Failed to create task: \(error.localizedDescription)"
        }
    }
}

protocol TaskServiceProtocol {
    func fetchAllTasks() async throws -> [TaskItem]
    func updateTask(_ task: TaskItem) async throws
    func createTask(_ task: TaskItem) async throws -> TaskItem
}
