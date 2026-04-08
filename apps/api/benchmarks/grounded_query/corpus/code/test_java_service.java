package com.example.inventory;

import java.util.List;
import java.util.Optional;
import java.util.stream.Collectors;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

/**
 * Represents a product in the inventory system.
 */
public class Product {
    private Long id;
    private String name;
    private String sku;
    private int quantity;
    private double price;

    public Product(Long id, String name, String sku, int quantity, double price) {
        this.id = id;
        this.name = name;
        this.sku = sku;
        this.quantity = quantity;
        this.price = price;
    }

    public Long getId() { return id; }
    public String getName() { return name; }
    public String getSku() { return sku; }
    public int getQuantity() { return quantity; }
    public void setQuantity(int quantity) { this.quantity = quantity; }
    public double getPrice() { return price; }
}

/**
 * Service responsible for managing product inventory,
 * including stock adjustments, lookups, and low-stock alerts.
 */
@Service
public class InventoryService {

    private final ProductRepository productRepository;
    private final NotificationService notificationService;

    private static final int LOW_STOCK_THRESHOLD = 10;

    public InventoryService(ProductRepository productRepository,
                            NotificationService notificationService) {
        this.productRepository = productRepository;
        this.notificationService = notificationService;
    }

    /**
     * Adjusts the stock quantity for a given product by the specified delta.
     * Sends a low-stock alert if the resulting quantity falls below the threshold.
     *
     * @param productId the ID of the product to adjust
     * @param delta     the quantity change (positive to add, negative to remove)
     * @return the updated product
     * @throws IllegalArgumentException if the product is not found
     * @throws IllegalStateException    if the adjustment would result in negative stock
     */
    @Transactional
    public Product adjustStock(Long productId, int delta) {
        Product product = productRepository.findById(productId)
                .orElseThrow(() -> new IllegalArgumentException(
                        "Product not found: " + productId));

        int newQuantity = product.getQuantity() + delta;
        if (newQuantity < 0) {
            throw new IllegalStateException(
                    "Insufficient stock for product " + product.getSku()
                    + ". Current: " + product.getQuantity() + ", requested: " + delta);
        }

        product.setQuantity(newQuantity);
        Product saved = productRepository.save(product);

        if (saved.getQuantity() < LOW_STOCK_THRESHOLD) {
            notificationService.sendLowStockAlert(saved);
        }

        return saved;
    }

    /**
     * Finds all products whose current stock is below the low-stock threshold.
     *
     * @return list of products with low stock, sorted by quantity ascending
     */
    public List<Product> findLowStockProducts() {
        return productRepository.findAll().stream()
                .filter(p -> p.getQuantity() < LOW_STOCK_THRESHOLD)
                .sorted((a, b) -> Integer.compare(a.getQuantity(), b.getQuantity()))
                .collect(Collectors.toList());
    }

    /**
     * Calculates the total monetary value of all inventory on hand.
     * Each product's value is quantity multiplied by unit price.
     *
     * @return total inventory value in the base currency
     */
    public double calculateTotalInventoryValue() {
        return productRepository.findAll().stream()
                .mapToDouble(p -> p.getQuantity() * p.getPrice())
                .sum();
    }
}
