using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Mvc;
using Microsoft.Extensions.Logging;

namespace BookStore.Api.Controllers
{
    /// <summary>
    /// Represents a book in the store catalog.
    /// </summary>
    public class Book
    {
        public int Id { get; set; }
        public string Title { get; set; }
        public string Author { get; set; }
        public string Isbn { get; set; }
        public decimal Price { get; set; }
        public int StockCount { get; set; }
        public DateTime PublishedDate { get; set; }
    }

    /// <summary>
    /// Manages the bookstore catalog, including listing, searching,
    /// and purchasing operations.
    /// </summary>
    [ApiController]
    [Route("api/[controller]")]
    public class BooksController : ControllerBase
    {
        private readonly IBookRepository _repository;
        private readonly ILogger<BooksController> _logger;

        public BooksController(IBookRepository repository, ILogger<BooksController> logger)
        {
            _repository = repository;
            _logger = logger;
        }

        /// <summary>
        /// Returns a paginated list of books, optionally filtered by author.
        /// Results are sorted by publication date descending.
        /// </summary>
        /// <param name="author">Optional author name filter (case-insensitive partial match).</param>
        /// <param name="page">Page number, starting from 1.</param>
        /// <param name="pageSize">Number of results per page (max 50).</param>
        /// <returns>A paginated list of books.</returns>
        [HttpGet]
        public async Task<ActionResult<PaginatedResult<Book>>> ListBooks(
            [FromQuery] string author = null,
            [FromQuery] int page = 1,
            [FromQuery] int pageSize = 20)
        {
            pageSize = Math.Clamp(pageSize, 1, 50);
            page = Math.Max(1, page);

            var query = await _repository.GetAllAsync();

            if (!string.IsNullOrWhiteSpace(author))
            {
                query = query.Where(b =>
                    b.Author.Contains(author, StringComparison.OrdinalIgnoreCase)).ToList();
            }

            var sorted = query.OrderByDescending(b => b.PublishedDate).ToList();
            var total = sorted.Count;
            var items = sorted.Skip((page - 1) * pageSize).Take(pageSize).ToList();

            _logger.LogInformation("Listed {Count} books (page {Page})", items.Count, page);

            return Ok(new PaginatedResult<Book>
            {
                Items = items,
                TotalCount = total,
                Page = page,
                PageSize = pageSize
            });
        }

        /// <summary>
        /// Retrieves a single book by its unique identifier.
        /// Returns 404 if the book does not exist.
        /// </summary>
        /// <param name="id">The book ID.</param>
        [HttpGet("{id}")]
        public async Task<ActionResult<Book>> GetBook(int id)
        {
            var book = await _repository.FindByIdAsync(id);
            if (book == null)
            {
                _logger.LogWarning("Book {Id} not found", id);
                return NotFound(new { message = $"Book with id {id} not found" });
            }
            return Ok(book);
        }

        /// <summary>
        /// Processes a purchase for the given book, decrementing stock.
        /// Returns 409 Conflict if the book is out of stock.
        /// </summary>
        /// <param name="id">The book ID to purchase.</param>
        /// <param name="quantity">Number of copies to purchase.</param>
        [HttpPost("{id}/purchase")]
        public async Task<ActionResult> PurchaseBook(int id, [FromBody] int quantity = 1)
        {
            var book = await _repository.FindByIdAsync(id);
            if (book == null)
                return NotFound();

            if (book.StockCount < quantity)
            {
                _logger.LogWarning("Insufficient stock for book {Id}: have {Stock}, want {Qty}",
                    id, book.StockCount, quantity);
                return Conflict(new { message = "Insufficient stock", available = book.StockCount });
            }

            book.StockCount -= quantity;
            await _repository.UpdateAsync(book);

            _logger.LogInformation("Purchased {Qty} of book {Id}", quantity, id);
            return Ok(new { message = "Purchase successful", remainingStock = book.StockCount });
        }
    }

    public class PaginatedResult<T>
    {
        public List<T> Items { get; set; }
        public int TotalCount { get; set; }
        public int Page { get; set; }
        public int PageSize { get; set; }
    }
}
