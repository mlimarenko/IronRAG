//! Order processing state machine with enums, traits, error handling,
//! serialization, state transitions, and validation rules.
//!
//! # Architecture decisions
//!
//! - Uses Rust enums for type-safe state representation.
//! - State transitions are validated at compile time where possible and
//!   at runtime via explicit transition tables.
//! - The `OrderStateMachine` trait defines the core interface that any
//!   state machine implementation must satisfy.
//! - Serde is used for JSON serialization of all domain types.
//! - Error handling uses a dedicated `OrderError` enum with specific variants.
//! - Events are recorded for each state transition for audit purposes.
//! - Configuration is loaded from environment variables (ORDER_TIMEOUT_SECONDS,
//!   MAX_RETRY_ATTEMPTS, PAYMENT_GATEWAY_URL, NOTIFICATION_SERVICE_URL,
//!   WAREHOUSE_API_URL, DATABASE_URL).

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// All possible errors that can occur during order processing.
/// Each variant carries context about the failure for debugging and logging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OrderError {
    /// The requested state transition is not allowed from the current state.
    InvalidTransition {
        from: OrderState,
        to: OrderState,
        reason: String,
    },
    /// A required field or condition was not met for the operation.
    ValidationError {
        field: String,
        message: String,
    },
    /// The payment processing step failed.
    PaymentFailed {
        order_id: String,
        gateway_code: String,
        message: String,
    },
    /// The inventory check or reservation failed.
    InventoryError {
        order_id: String,
        sku: String,
        requested: u32,
        available: u32,
    },
    /// A shipping or fulfillment operation failed.
    ShippingError {
        order_id: String,
        carrier: String,
        message: String,
    },
    /// The order was not found in the system.
    OrderNotFound {
        order_id: String,
    },
    /// An internal system error occurred.
    InternalError {
        message: String,
    },
    /// The operation timed out.
    Timeout {
        operation: String,
        duration_seconds: u64,
    },
    /// A refund processing error occurred.
    RefundError {
        order_id: String,
        amount_cents: u64,
        reason: String,
    },
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderError::InvalidTransition { from, to, reason } => {
                write!(f, "Invalid transition from {:?} to {:?}: {}", from, to, reason)
            }
            OrderError::ValidationError { field, message } => {
                write!(f, "Validation error on '{}': {}", field, message)
            }
            OrderError::PaymentFailed { order_id, gateway_code, message } => {
                write!(f, "Payment failed for order {}: [{}] {}", order_id, gateway_code, message)
            }
            OrderError::InventoryError { order_id, sku, requested, available } => {
                write!(f, "Inventory error for order {}: SKU {} requested {} but only {} available", order_id, sku, requested, available)
            }
            OrderError::ShippingError { order_id, carrier, message } => {
                write!(f, "Shipping error for order {} via {}: {}", order_id, carrier, message)
            }
            OrderError::OrderNotFound { order_id } => {
                write!(f, "Order {} not found", order_id)
            }
            OrderError::InternalError { message } => {
                write!(f, "Internal error: {}", message)
            }
            OrderError::Timeout { operation, duration_seconds } => {
                write!(f, "Operation '{}' timed out after {}s", operation, duration_seconds)
            }
            OrderError::RefundError { order_id, amount_cents, reason } => {
                write!(f, "Refund error for order {} ({}c): {}", order_id, amount_cents, reason)
            }
        }
    }
}

impl std::error::Error for OrderError {}

// ---------------------------------------------------------------------------
// State and event enums
// ---------------------------------------------------------------------------

/// All possible states an order can be in.
///
/// The valid state transitions are:
///
/// - `Pending` -> `PaymentProcessing`, `Cancelled`
/// - `PaymentProcessing` -> `PaymentConfirmed`, `PaymentFailed`
/// - `PaymentConfirmed` -> `InventoryReserved`, `Cancelled`
/// - `PaymentFailed` -> `Pending` (retry), `Cancelled`
/// - `InventoryReserved` -> `Shipped`, `Cancelled`
/// - `Shipped` -> `Delivered`, `ReturnRequested`
/// - `Delivered` -> `ReturnRequested`, `Completed`
/// - `ReturnRequested` -> `ReturnApproved`, `ReturnRejected`
/// - `ReturnApproved` -> `Refunded`
/// - `ReturnRejected` -> `Completed`
/// - `Refunded` -> (terminal)
/// - `Cancelled` -> (terminal)
/// - `Completed` -> (terminal)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderState {
    Pending,
    PaymentProcessing,
    PaymentConfirmed,
    PaymentFailed,
    InventoryReserved,
    Shipped,
    Delivered,
    ReturnRequested,
    ReturnApproved,
    ReturnRejected,
    Refunded,
    Cancelled,
    Completed,
}

impl OrderState {
    /// Returns the list of valid target states from this state.
    pub fn valid_transitions(&self) -> &[OrderState] {
        match self {
            OrderState::Pending => &[OrderState::PaymentProcessing, OrderState::Cancelled],
            OrderState::PaymentProcessing => &[OrderState::PaymentConfirmed, OrderState::PaymentFailed],
            OrderState::PaymentConfirmed => &[OrderState::InventoryReserved, OrderState::Cancelled],
            OrderState::PaymentFailed => &[OrderState::Pending, OrderState::Cancelled],
            OrderState::InventoryReserved => &[OrderState::Shipped, OrderState::Cancelled],
            OrderState::Shipped => &[OrderState::Delivered, OrderState::ReturnRequested],
            OrderState::Delivered => &[OrderState::ReturnRequested, OrderState::Completed],
            OrderState::ReturnRequested => &[OrderState::ReturnApproved, OrderState::ReturnRejected],
            OrderState::ReturnApproved => &[OrderState::Refunded],
            OrderState::ReturnRejected => &[OrderState::Completed],
            OrderState::Refunded | OrderState::Cancelled | OrderState::Completed => &[],
        }
    }

    /// Returns true if this is a terminal (final) state.
    pub fn is_terminal(&self) -> bool {
        self.valid_transitions().is_empty()
    }

    /// Returns true if the transition to the target state is valid.
    pub fn can_transition_to(&self, target: OrderState) -> bool {
        self.valid_transitions().contains(&target)
    }
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Events that trigger state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrderEvent {
    Submit,
    ProcessPayment { payment_method: String, amount_cents: u64 },
    ConfirmPayment { transaction_id: String },
    FailPayment { error_code: String, message: String },
    ReserveInventory,
    Ship { carrier: String, tracking_number: String },
    ConfirmDelivery,
    RequestReturn { reason: String },
    ApproveReturn { approved_by: String },
    RejectReturn { rejected_by: String, reason: String },
    ProcessRefund { amount_cents: u64 },
    Cancel { reason: String, cancelled_by: String },
    Complete,
    RetryPayment,
}

// ---------------------------------------------------------------------------
// Domain models
// ---------------------------------------------------------------------------

/// Represents a line item in an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub sku: String,
    pub name: String,
    pub quantity: u32,
    pub unit_price_cents: u64,
    pub weight_grams: u32,
}

impl OrderItem {
    pub fn total_price_cents(&self) -> u64 {
        self.unit_price_cents * self.quantity as u64
    }
}

/// Shipping address for an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShippingAddress {
    pub line1: String,
    pub line2: Option<String>,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    pub country: String,
}

impl ShippingAddress {
    /// Validate the shipping address fields.
    pub fn validate(&self) -> Result<(), OrderError> {
        if self.line1.is_empty() {
            return Err(OrderError::ValidationError {
                field: "line1".to_string(),
                message: "Address line 1 is required".to_string(),
            });
        }
        if self.city.is_empty() {
            return Err(OrderError::ValidationError {
                field: "city".to_string(),
                message: "City is required".to_string(),
            });
        }
        if self.postal_code.is_empty() {
            return Err(OrderError::ValidationError {
                field: "postal_code".to_string(),
                message: "Postal code is required".to_string(),
            });
        }
        if self.country.len() != 2 {
            return Err(OrderError::ValidationError {
                field: "country".to_string(),
                message: "Country must be a 2-letter ISO code".to_string(),
            });
        }
        Ok(())
    }
}

/// Payment information for an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentInfo {
    pub method: String,
    pub transaction_id: Option<String>,
    pub amount_cents: u64,
    pub currency: String,
    pub status: String,
    pub processed_at: Option<u64>,
}

/// Shipping information for a fulfilled order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShippingInfo {
    pub carrier: String,
    pub tracking_number: String,
    pub shipped_at: u64,
    pub estimated_delivery: Option<u64>,
    pub delivered_at: Option<u64>,
}

/// Audit event recorded for each state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionEvent {
    pub from_state: OrderState,
    pub to_state: OrderState,
    pub event: OrderEvent,
    pub timestamp: u64,
    pub actor: Option<String>,
    pub metadata: HashMap<String, String>,
}

/// The complete order entity with all associated data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub customer_id: String,
    pub state: OrderState,
    pub items: Vec<OrderItem>,
    pub shipping_address: ShippingAddress,
    pub payment: Option<PaymentInfo>,
    pub shipping: Option<ShippingInfo>,
    pub subtotal_cents: u64,
    pub tax_cents: u64,
    pub shipping_cost_cents: u64,
    pub total_cents: u64,
    pub currency: String,
    pub notes: Option<String>,
    pub history: Vec<TransitionEvent>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl Order {
    /// Create a new order in the Pending state.
    pub fn new(
        id: String,
        customer_id: String,
        items: Vec<OrderItem>,
        shipping_address: ShippingAddress,
        currency: String,
    ) -> Result<Self, OrderError> {
        if items.is_empty() {
            return Err(OrderError::ValidationError {
                field: "items".to_string(),
                message: "Order must contain at least one item".to_string(),
            });
        }
        shipping_address.validate()?;

        let subtotal_cents: u64 = items.iter().map(|i| i.total_price_cents()).sum();
        let tax_cents = (subtotal_cents as f64 * 0.08) as u64; // 8% tax
        let total_weight: u32 = items.iter().map(|i| i.weight_grams * i.quantity).sum();
        let shipping_cost_cents = calculate_shipping(total_weight, &shipping_address.country);
        let total_cents = subtotal_cents + tax_cents + shipping_cost_cents;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Order {
            id,
            customer_id,
            state: OrderState::Pending,
            items,
            shipping_address,
            payment: None,
            shipping: None,
            subtotal_cents,
            tax_cents,
            shipping_cost_cents,
            total_cents,
            currency,
            notes: None,
            history: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Calculate the total number of items in the order.
    pub fn total_items(&self) -> u32 {
        self.items.iter().map(|i| i.quantity).sum()
    }

    /// Record a state transition event in the order history.
    fn record_transition(&mut self, from: OrderState, to: OrderState, event: OrderEvent, actor: Option<String>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.history.push(TransitionEvent {
            from_state: from,
            to_state: to,
            event,
            timestamp: now,
            actor,
            metadata: HashMap::new(),
        });
        self.updated_at = now;
    }
}

/// Calculate shipping cost based on weight and destination country.
fn calculate_shipping(weight_grams: u32, country: &str) -> u64 {
    let base_rate: u64 = match country {
        "US" => 599,
        "CA" | "MX" => 999,
        "GB" | "DE" | "FR" => 1499,
        _ => 1999,
    };
    let weight_surcharge = (weight_grams as u64 / 500) * 200;
    base_rate + weight_surcharge
}

// ---------------------------------------------------------------------------
// State machine trait
// ---------------------------------------------------------------------------

/// Defines the interface for order state machine implementations.
///
/// Implementors must handle state transitions, validation, and side effects
/// (payment processing, inventory reservation, shipping, etc.).
pub trait OrderStateMachine {
    /// Attempt to transition the order to a new state via the given event.
    /// Returns the new state on success or an error if the transition is invalid.
    fn transition(&mut self, event: OrderEvent) -> Result<OrderState, OrderError>;

    /// Get the current state of the order.
    fn current_state(&self) -> OrderState;

    /// Check if the order is in a terminal state.
    fn is_complete(&self) -> bool;

    /// Get the full transition history.
    fn history(&self) -> &[TransitionEvent];

    /// Validate the order for a specific transition.
    fn validate_transition(&self, target: OrderState) -> Result<(), OrderError>;
}

/// Defines hooks for side effects during state transitions.
pub trait TransitionHooks {
    /// Called before a state transition is applied. Can veto the transition
    /// by returning an error.
    fn before_transition(&self, order: &Order, from: OrderState, to: OrderState) -> Result<(), OrderError>;

    /// Called after a state transition has been applied. Used for side effects
    /// like sending notifications, updating external systems, etc.
    fn after_transition(&self, order: &Order, from: OrderState, to: OrderState);
}

/// Serialization trait for persisting orders to storage.
pub trait OrderRepository {
    /// Save or update an order in the repository.
    fn save(&self, order: &Order) -> Result<(), OrderError>;

    /// Load an order by its ID.
    fn find_by_id(&self, id: &str) -> Result<Option<Order>, OrderError>;

    /// Find all orders for a customer.
    fn find_by_customer(&self, customer_id: &str) -> Result<Vec<Order>, OrderError>;

    /// Find orders by state for processing.
    fn find_by_state(&self, state: OrderState) -> Result<Vec<Order>, OrderError>;
}

// ---------------------------------------------------------------------------
// State machine implementation
// ---------------------------------------------------------------------------

/// Production implementation of the order state machine.
pub struct OrderProcessor {
    order: Order,
}

impl OrderProcessor {
    /// Create a new processor wrapping an existing order.
    pub fn new(order: Order) -> Self {
        OrderProcessor { order }
    }

    /// Get a reference to the underlying order.
    pub fn order(&self) -> &Order {
        &self.order
    }

    /// Consume the processor and return the order.
    pub fn into_order(self) -> Order {
        self.order
    }
}

impl OrderStateMachine for OrderProcessor {
    fn transition(&mut self, event: OrderEvent) -> Result<OrderState, OrderError> {
        let current = self.order.state;
        let target = match (&current, &event) {
            (OrderState::Pending, OrderEvent::ProcessPayment { .. }) => OrderState::PaymentProcessing,
            (OrderState::Pending, OrderEvent::Cancel { .. }) => OrderState::Cancelled,
            (OrderState::PaymentProcessing, OrderEvent::ConfirmPayment { .. }) => OrderState::PaymentConfirmed,
            (OrderState::PaymentProcessing, OrderEvent::FailPayment { .. }) => OrderState::PaymentFailed,
            (OrderState::PaymentConfirmed, OrderEvent::ReserveInventory) => OrderState::InventoryReserved,
            (OrderState::PaymentConfirmed, OrderEvent::Cancel { .. }) => OrderState::Cancelled,
            (OrderState::PaymentFailed, OrderEvent::RetryPayment) => OrderState::Pending,
            (OrderState::PaymentFailed, OrderEvent::Cancel { .. }) => OrderState::Cancelled,
            (OrderState::InventoryReserved, OrderEvent::Ship { .. }) => OrderState::Shipped,
            (OrderState::InventoryReserved, OrderEvent::Cancel { .. }) => OrderState::Cancelled,
            (OrderState::Shipped, OrderEvent::ConfirmDelivery) => OrderState::Delivered,
            (OrderState::Shipped, OrderEvent::RequestReturn { .. }) => OrderState::ReturnRequested,
            (OrderState::Delivered, OrderEvent::RequestReturn { .. }) => OrderState::ReturnRequested,
            (OrderState::Delivered, OrderEvent::Complete) => OrderState::Completed,
            (OrderState::ReturnRequested, OrderEvent::ApproveReturn { .. }) => OrderState::ReturnApproved,
            (OrderState::ReturnRequested, OrderEvent::RejectReturn { .. }) => OrderState::ReturnRejected,
            (OrderState::ReturnApproved, OrderEvent::ProcessRefund { .. }) => OrderState::Refunded,
            (OrderState::ReturnRejected, OrderEvent::Complete) => OrderState::Completed,
            _ => {
                return Err(OrderError::InvalidTransition {
                    from: current,
                    to: OrderState::Pending, // placeholder
                    reason: format!("Event {:?} not valid in state {:?}", event, current),
                });
            }
        };

        self.validate_transition(target)?;

        // Apply side effects based on the event
        match &event {
            OrderEvent::ProcessPayment { payment_method, amount_cents } => {
                self.order.payment = Some(PaymentInfo {
                    method: payment_method.clone(),
                    transaction_id: None,
                    amount_cents: *amount_cents,
                    currency: self.order.currency.clone(),
                    status: "processing".to_string(),
                    processed_at: None,
                });
            }
            OrderEvent::ConfirmPayment { transaction_id } => {
                if let Some(ref mut payment) = self.order.payment {
                    payment.transaction_id = Some(transaction_id.clone());
                    payment.status = "confirmed".to_string();
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                    payment.processed_at = Some(now);
                }
            }
            OrderEvent::FailPayment { error_code, message } => {
                if let Some(ref mut payment) = self.order.payment {
                    payment.status = format!("failed:{}", error_code);
                }
            }
            OrderEvent::Ship { carrier, tracking_number } => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                self.order.shipping = Some(ShippingInfo {
                    carrier: carrier.clone(),
                    tracking_number: tracking_number.clone(),
                    shipped_at: now,
                    estimated_delivery: Some(now + 5 * 86400), // 5 days
                    delivered_at: None,
                });
            }
            OrderEvent::ConfirmDelivery => {
                if let Some(ref mut shipping) = self.order.shipping {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                    shipping.delivered_at = Some(now);
                }
            }
            _ => {}
        }

        let previous = self.order.state;
        self.order.state = target;
        self.order.record_transition(previous, target, event, None);

        Ok(target)
    }

    fn current_state(&self) -> OrderState {
        self.order.state
    }

    fn is_complete(&self) -> bool {
        self.order.state.is_terminal()
    }

    fn history(&self) -> &[TransitionEvent] {
        &self.order.history
    }

    fn validate_transition(&self, target: OrderState) -> Result<(), OrderError> {
        let current = self.order.state;

        if !current.can_transition_to(target) {
            return Err(OrderError::InvalidTransition {
                from: current,
                to: target,
                reason: format!(
                    "Valid transitions from {:?}: {:?}",
                    current,
                    current.valid_transitions()
                ),
            });
        }

        // Additional business validations
        match target {
            OrderState::PaymentProcessing => {
                if self.order.total_cents == 0 {
                    return Err(OrderError::ValidationError {
                        field: "total_cents".to_string(),
                        message: "Order total must be greater than zero".to_string(),
                    });
                }
            }
            OrderState::Shipped => {
                if self.order.shipping_address.line1.is_empty() {
                    return Err(OrderError::ValidationError {
                        field: "shipping_address".to_string(),
                        message: "Shipping address is required before shipping".to_string(),
                    });
                }
            }
            OrderState::Refunded => {
                if self.order.payment.is_none() {
                    return Err(OrderError::ValidationError {
                        field: "payment".to_string(),
                        message: "Cannot refund an order with no payment".to_string(),
                    });
                }
            }
            _ => {}
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the order processing system, loaded from environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderConfig {
    /// ORDER_TIMEOUT_SECONDS - Time before a pending order is auto-cancelled. Default: 3600
    pub order_timeout_seconds: u64,
    /// MAX_RETRY_ATTEMPTS - Maximum payment retry attempts. Default: 3
    pub max_retry_attempts: u32,
    /// PAYMENT_GATEWAY_URL - URL of the payment gateway API.
    pub payment_gateway_url: String,
    /// NOTIFICATION_SERVICE_URL - URL of the notification service.
    pub notification_service_url: String,
    /// WAREHOUSE_API_URL - URL of the warehouse/fulfillment API.
    pub warehouse_api_url: String,
    /// DATABASE_URL - PostgreSQL connection string.
    pub database_url: String,
}

impl OrderConfig {
    /// Load configuration from environment variables.
    pub fn from_env() -> Self {
        OrderConfig {
            order_timeout_seconds: std::env::var("ORDER_TIMEOUT_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600),
            max_retry_attempts: std::env::var("MAX_RETRY_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            payment_gateway_url: std::env::var("PAYMENT_GATEWAY_URL")
                .unwrap_or_default(),
            notification_service_url: std::env::var("NOTIFICATION_SERVICE_URL")
                .unwrap_or_default(),
            warehouse_api_url: std::env::var("WAREHOUSE_API_URL")
                .unwrap_or_default(),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_order() -> Order {
        Order::new(
            "ord_001".to_string(),
            "cust_001".to_string(),
            vec![OrderItem {
                sku: "SKU-001".to_string(),
                name: "Widget".to_string(),
                quantity: 2,
                unit_price_cents: 1500,
                weight_grams: 250,
            }],
            ShippingAddress {
                line1: "123 Main St".to_string(),
                line2: None,
                city: "Portland".to_string(),
                state: "OR".to_string(),
                postal_code: "97201".to_string(),
                country: "US".to_string(),
            },
            "USD".to_string(),
        )
        .unwrap()
    }

    #[test]
    fn test_order_creation() {
        let order = make_test_order();
        assert_eq!(order.state, OrderState::Pending);
        assert_eq!(order.total_items(), 2);
        assert_eq!(order.subtotal_cents, 3000);
        assert!(order.total_cents > order.subtotal_cents); // includes tax and shipping
    }

    #[test]
    fn test_order_creation_empty_items() {
        let result = Order::new(
            "ord_002".to_string(),
            "cust_001".to_string(),
            vec![],
            ShippingAddress {
                line1: "123 Main St".to_string(),
                line2: None,
                city: "Portland".to_string(),
                state: "OR".to_string(),
                postal_code: "97201".to_string(),
                country: "US".to_string(),
            },
            "USD".to_string(),
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            OrderError::ValidationError { field, .. } => assert_eq!(field, "items"),
            _ => panic!("Expected ValidationError"),
        }
    }

    #[test]
    fn test_valid_transitions_from_pending() {
        let transitions = OrderState::Pending.valid_transitions();
        assert!(transitions.contains(&OrderState::PaymentProcessing));
        assert!(transitions.contains(&OrderState::Cancelled));
        assert!(!transitions.contains(&OrderState::Shipped));
        assert!(!transitions.contains(&OrderState::Delivered));
    }

    #[test]
    fn test_happy_path_order_flow() {
        let order = make_test_order();
        let mut processor = OrderProcessor::new(order);

        // Pending -> PaymentProcessing
        let state = processor.transition(OrderEvent::ProcessPayment {
            payment_method: "credit_card".to_string(),
            amount_cents: 3000,
        }).unwrap();
        assert_eq!(state, OrderState::PaymentProcessing);

        // PaymentProcessing -> PaymentConfirmed
        let state = processor.transition(OrderEvent::ConfirmPayment {
            transaction_id: "txn_abc123".to_string(),
        }).unwrap();
        assert_eq!(state, OrderState::PaymentConfirmed);

        // PaymentConfirmed -> InventoryReserved
        let state = processor.transition(OrderEvent::ReserveInventory).unwrap();
        assert_eq!(state, OrderState::InventoryReserved);

        // InventoryReserved -> Shipped
        let state = processor.transition(OrderEvent::Ship {
            carrier: "FedEx".to_string(),
            tracking_number: "TRACK123".to_string(),
        }).unwrap();
        assert_eq!(state, OrderState::Shipped);

        // Shipped -> Delivered
        let state = processor.transition(OrderEvent::ConfirmDelivery).unwrap();
        assert_eq!(state, OrderState::Delivered);

        // Delivered -> Completed
        let state = processor.transition(OrderEvent::Complete).unwrap();
        assert_eq!(state, OrderState::Completed);

        assert!(processor.is_complete());
        assert_eq!(processor.history().len(), 6);
    }

    #[test]
    fn test_invalid_transition_rejected() {
        let order = make_test_order();
        let mut processor = OrderProcessor::new(order);

        // Cannot ship from Pending
        let result = processor.transition(OrderEvent::Ship {
            carrier: "UPS".to_string(),
            tracking_number: "T001".to_string(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_payment_failure_and_retry() {
        let order = make_test_order();
        let mut processor = OrderProcessor::new(order);

        // Start payment
        processor.transition(OrderEvent::ProcessPayment {
            payment_method: "credit_card".to_string(),
            amount_cents: 3000,
        }).unwrap();

        // Payment fails
        processor.transition(OrderEvent::FailPayment {
            error_code: "DECLINED".to_string(),
            message: "Insufficient funds".to_string(),
        }).unwrap();
        assert_eq!(processor.current_state(), OrderState::PaymentFailed);

        // Retry payment (goes back to Pending)
        processor.transition(OrderEvent::RetryPayment).unwrap();
        assert_eq!(processor.current_state(), OrderState::Pending);
    }

    #[test]
    fn test_return_flow() {
        let order = make_test_order();
        let mut processor = OrderProcessor::new(order);

        // Fast-forward to Delivered
        processor.transition(OrderEvent::ProcessPayment {
            payment_method: "credit_card".to_string(),
            amount_cents: 3000,
        }).unwrap();
        processor.transition(OrderEvent::ConfirmPayment {
            transaction_id: "txn_001".to_string(),
        }).unwrap();
        processor.transition(OrderEvent::ReserveInventory).unwrap();
        processor.transition(OrderEvent::Ship {
            carrier: "UPS".to_string(),
            tracking_number: "T001".to_string(),
        }).unwrap();
        processor.transition(OrderEvent::ConfirmDelivery).unwrap();

        // Request return
        processor.transition(OrderEvent::RequestReturn {
            reason: "Wrong size".to_string(),
        }).unwrap();
        assert_eq!(processor.current_state(), OrderState::ReturnRequested);

        // Approve return
        processor.transition(OrderEvent::ApproveReturn {
            approved_by: "admin_001".to_string(),
        }).unwrap();
        assert_eq!(processor.current_state(), OrderState::ReturnApproved);

        // Process refund
        processor.transition(OrderEvent::ProcessRefund {
            amount_cents: 3000,
        }).unwrap();
        assert_eq!(processor.current_state(), OrderState::Refunded);
        assert!(processor.is_complete());
    }

    #[test]
    fn test_cancellation_from_multiple_states() {
        // Test cancellation from Pending
        let order = make_test_order();
        let mut processor = OrderProcessor::new(order);
        processor.transition(OrderEvent::Cancel {
            reason: "Changed mind".to_string(),
            cancelled_by: "customer".to_string(),
        }).unwrap();
        assert_eq!(processor.current_state(), OrderState::Cancelled);

        // Test cancellation from PaymentFailed
        let order2 = make_test_order();
        let mut processor2 = OrderProcessor::new(order2);
        processor2.transition(OrderEvent::ProcessPayment {
            payment_method: "credit_card".to_string(),
            amount_cents: 3000,
        }).unwrap();
        processor2.transition(OrderEvent::FailPayment {
            error_code: "DECLINED".to_string(),
            message: "Card declined".to_string(),
        }).unwrap();
        processor2.transition(OrderEvent::Cancel {
            reason: "Too many failures".to_string(),
            cancelled_by: "system".to_string(),
        }).unwrap();
        assert_eq!(processor2.current_state(), OrderState::Cancelled);
    }

    #[test]
    fn test_terminal_states() {
        assert!(OrderState::Refunded.is_terminal());
        assert!(OrderState::Cancelled.is_terminal());
        assert!(OrderState::Completed.is_terminal());
        assert!(!OrderState::Pending.is_terminal());
        assert!(!OrderState::Shipped.is_terminal());
    }

    #[test]
    fn test_shipping_address_validation() {
        let invalid = ShippingAddress {
            line1: "".to_string(),
            line2: None,
            city: "Portland".to_string(),
            state: "OR".to_string(),
            postal_code: "97201".to_string(),
            country: "US".to_string(),
        };
        assert!(invalid.validate().is_err());

        let invalid_country = ShippingAddress {
            line1: "123 Main".to_string(),
            line2: None,
            city: "Portland".to_string(),
            state: "OR".to_string(),
            postal_code: "97201".to_string(),
            country: "USA".to_string(), // must be 2-letter
        };
        assert!(invalid_country.validate().is_err());
    }

    #[test]
    fn test_order_serialization() {
        let order = make_test_order();
        let json = serde_json::to_string(&order).unwrap();
        let deserialized: Order = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, order.id);
        assert_eq!(deserialized.state, order.state);
        assert_eq!(deserialized.items.len(), order.items.len());
    }

    #[test]
    fn test_shipping_cost_calculation() {
        assert_eq!(calculate_shipping(500, "US"), 799); // 599 + 200
        assert_eq!(calculate_shipping(250, "US"), 599); // 599 + 0 (under 500g)
        assert_eq!(calculate_shipping(1000, "CA"), 1399); // 999 + 400
        assert_eq!(calculate_shipping(200, "JP"), 1999); // default international
    }
}
