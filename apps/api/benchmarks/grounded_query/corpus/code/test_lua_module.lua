--- A module for managing an in-memory key-value store with expiration.
--- Provides get, set, delete, and cleanup operations.

local KeyValueStore = {}
KeyValueStore.__index = KeyValueStore

--- Entry structure stored internally.
--- @class Entry
--- @field value any The stored value
--- @field expires_at number|nil Expiration timestamp, or nil for no expiry

--- Creates a new KeyValueStore instance.
--- @param max_size number Maximum number of entries (default 10000)
--- @return table A new KeyValueStore
function KeyValueStore.new(max_size)
    local self = setmetatable({}, KeyValueStore)
    self._store = {}
    self._count = 0
    self._max_size = max_size or 10000
    self._stats = { hits = 0, misses = 0, evictions = 0 }
    return self
end

--- Retrieves a value by key. Returns nil if the key does not exist
--- or has expired. Expired entries are lazily removed on access.
---
--- @param key string The key to look up
--- @return any|nil The stored value, or nil if not found/expired
function KeyValueStore:get(key)
    local entry = self._store[key]

    if entry == nil then
        self._stats.misses = self._stats.misses + 1
        return nil
    end

    if entry.expires_at and os.time() > entry.expires_at then
        self._store[key] = nil
        self._count = self._count - 1
        self._stats.misses = self._stats.misses + 1
        return nil
    end

    self._stats.hits = self._stats.hits + 1
    return entry.value
end

--- Stores a value under the given key with an optional TTL in seconds.
--- If the store is at capacity, the oldest entry is evicted first.
---
--- @param key string The key to store under
--- @param value any The value to store
--- @param ttl_seconds number|nil Optional time-to-live in seconds
--- @return boolean True if stored successfully
function KeyValueStore:set(key, value, ttl_seconds)
    if self._store[key] == nil and self._count >= self._max_size then
        self:_evict_oldest()
    end

    local expires_at = nil
    if ttl_seconds and ttl_seconds > 0 then
        expires_at = os.time() + ttl_seconds
    end

    if self._store[key] == nil then
        self._count = self._count + 1
    end

    self._store[key] = {
        value = value,
        expires_at = expires_at,
        created_at = os.time()
    }

    return true
end

--- Deletes a key from the store.
--- Returns true if the key existed, false otherwise.
---
--- @param key string The key to delete
--- @return boolean Whether the key was found and deleted
function KeyValueStore:delete(key)
    if self._store[key] then
        self._store[key] = nil
        self._count = self._count - 1
        return true
    end
    return false
end

--- Removes all expired entries from the store.
--- Returns the number of entries removed.
---
--- @return number Count of evicted entries
function KeyValueStore:cleanup_expired()
    local now = os.time()
    local removed = 0

    for key, entry in pairs(self._store) do
        if entry.expires_at and now > entry.expires_at then
            self._store[key] = nil
            self._count = self._count - 1
            removed = removed + 1
        end
    end

    self._stats.evictions = self._stats.evictions + removed
    return removed
end

--- Returns current store statistics including hit/miss ratio.
---
--- @return table Stats with hits, misses, evictions, size, and hit_rate
function KeyValueStore:stats()
    local total = self._stats.hits + self._stats.misses
    local hit_rate = 0
    if total > 0 then
        hit_rate = self._stats.hits / total
    end

    return {
        size = self._count,
        max_size = self._max_size,
        hits = self._stats.hits,
        misses = self._stats.misses,
        evictions = self._stats.evictions,
        hit_rate = hit_rate
    }
end

--- Evicts the oldest entry from the store based on created_at timestamp.
function KeyValueStore:_evict_oldest()
    local oldest_key = nil
    local oldest_time = math.huge

    for key, entry in pairs(self._store) do
        if entry.created_at < oldest_time then
            oldest_time = entry.created_at
            oldest_key = key
        end
    end

    if oldest_key then
        self._store[oldest_key] = nil
        self._count = self._count - 1
        self._stats.evictions = self._stats.evictions + 1
    end
end

return KeyValueStore
