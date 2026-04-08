<?php

namespace App\Models;

use Illuminate\Database\Eloquent\Model;
use Illuminate\Database\Eloquent\Builder;
use Illuminate\Database\Eloquent\Relations\HasMany;
use Illuminate\Database\Eloquent\Relations\BelongsTo;
use Carbon\Carbon;

/**
 * Represents an order in the e-commerce system.
 *
 * @property int $id
 * @property int $customer_id
 * @property string $status
 * @property float $total_amount
 * @property Carbon $placed_at
 * @property Carbon $shipped_at
 */
class Order extends Model
{
    protected $fillable = [
        'customer_id',
        'status',
        'total_amount',
        'placed_at',
        'shipped_at',
    ];

    protected $casts = [
        'total_amount' => 'float',
        'placed_at' => 'datetime',
        'shipped_at' => 'datetime',
    ];

    const STATUS_PENDING = 'pending';
    const STATUS_CONFIRMED = 'confirmed';
    const STATUS_SHIPPED = 'shipped';
    const STATUS_DELIVERED = 'delivered';
    const STATUS_CANCELLED = 'cancelled';

    public function customer(): BelongsTo
    {
        return $this->belongsTo(Customer::class);
    }

    public function lineItems(): HasMany
    {
        return $this->hasMany(OrderLineItem::class);
    }

    /**
     * Scope to filter orders by their current status.
     * Can be chained with other query scopes.
     *
     * @param Builder $query
     * @param string $status One of the STATUS_* constants.
     * @return Builder
     */
    public function scopeWithStatus(Builder $query, string $status): Builder
    {
        return $query->where('status', $status);
    }

    /**
     * Scope to find orders placed within a given date range.
     * Both start and end dates are inclusive.
     *
     * @param Builder $query
     * @param Carbon $start Start of the date range.
     * @param Carbon $end   End of the date range.
     * @return Builder
     */
    public function scopePlacedBetween(Builder $query, Carbon $start, Carbon $end): Builder
    {
        return $query->whereBetween('placed_at', [$start->startOfDay(), $end->endOfDay()]);
    }

    /**
     * Scope to find high-value orders above a given threshold.
     *
     * @param Builder $query
     * @param float $minAmount Minimum total amount.
     * @return Builder
     */
    public function scopeHighValue(Builder $query, float $minAmount = 500.0): Builder
    {
        return $query->where('total_amount', '>=', $minAmount);
    }

    /**
     * Marks the order as shipped, setting the shipped_at timestamp
     * and updating the status. Throws if the order is not in confirmed status.
     *
     * @return self
     * @throws \LogicException If the order cannot be shipped from its current status.
     */
    public function markAsShipped(): self
    {
        if ($this->status !== self::STATUS_CONFIRMED) {
            throw new \LogicException(
                "Cannot ship order #{$this->id}: current status is '{$this->status}', expected 'confirmed'."
            );
        }

        $this->update([
            'status' => self::STATUS_SHIPPED,
            'shipped_at' => Carbon::now(),
        ]);

        return $this;
    }

    /**
     * Recalculates the order total from its line items.
     * Updates the total_amount field and persists the change.
     *
     * @return float The recalculated total.
     */
    public function recalculateTotal(): float
    {
        $total = $this->lineItems()
            ->get()
            ->sum(fn ($item) => $item->quantity * $item->unit_price);

        $this->update(['total_amount' => $total]);

        return $total;
    }
}
