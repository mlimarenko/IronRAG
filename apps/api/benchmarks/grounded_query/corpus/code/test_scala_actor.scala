package com.example.cache

import akka.actor.{Actor, ActorLogging, Props}
import scala.collection.mutable
import scala.concurrent.duration._

/**
 * Represents an entry in the cache with its value,
 * insertion time, and optional time-to-live duration.
 */
case class CacheEntry(
  key: String,
  value: Any,
  insertedAt: Long = System.currentTimeMillis(),
  ttlMillis: Option[Long] = None
) {
  /** Returns true if this entry has exceeded its TTL. */
  def isExpired: Boolean = ttlMillis.exists { ttl =>
    System.currentTimeMillis() - insertedAt > ttl
  }
}

/** Messages accepted by the CacheActor. */
object CacheActor {
  def props(maxSize: Int = 1000): Props = Props(new CacheActor(maxSize))

  sealed trait CacheMessage
  case class Put(key: String, value: Any, ttl: Option[FiniteDuration] = None) extends CacheMessage
  case class Get(key: String) extends CacheMessage
  case class Remove(key: String) extends CacheMessage
  case object GetStats extends CacheMessage
  case object EvictExpired extends CacheMessage

  case class CacheStats(size: Int, hits: Long, misses: Long, evictions: Long)
}

/**
 * An in-memory cache actor that stores key-value pairs with optional TTL.
 * Supports LRU eviction when the cache reaches its maximum capacity.
 * Periodically evicts expired entries when prompted.
 */
class CacheActor(maxSize: Int) extends Actor with ActorLogging {
  import CacheActor._

  private val store = mutable.LinkedHashMap.empty[String, CacheEntry]
  private var hits: Long = 0
  private var misses: Long = 0
  private var evictions: Long = 0

  /**
   * Handles incoming cache messages.
   * Put inserts or updates an entry, evicting the oldest if at capacity.
   * Get retrieves a value if present and not expired.
   * Remove deletes a specific key.
   * GetStats returns current cache statistics.
   * EvictExpired removes all entries that have exceeded their TTL.
   */
  override def receive: Receive = {
    case Put(key, value, ttl) =>
      handlePut(key, value, ttl)

    case Get(key) =>
      handleGet(key)

    case Remove(key) =>
      val existed = store.remove(key).isDefined
      sender() ! existed

    case GetStats =>
      sender() ! CacheStats(store.size, hits, misses, evictions)

    case EvictExpired =>
      handleEvictExpired()
  }

  /**
   * Inserts or updates a cache entry. If the cache is at maximum
   * capacity and the key is new, the oldest entry is evicted first.
   *
   * @param key   The cache key.
   * @param value The value to store.
   * @param ttl   Optional time-to-live for the entry.
   */
  private def handlePut(key: String, value: Any, ttl: Option[FiniteDuration]): Unit = {
    store.remove(key)

    if (store.size >= maxSize) {
      val oldest = store.head._1
      store.remove(oldest)
      evictions += 1
      log.debug("Evicted oldest entry: {}", oldest)
    }

    val entry = CacheEntry(key, value, ttlMillis = ttl.map(_.toMillis))
    store.put(key, entry)
    sender() ! true
  }

  /**
   * Retrieves a cached value by key. Returns Some(value) if the key
   * exists and has not expired, otherwise returns None.
   * Expired entries are removed on access.
   *
   * @param key The cache key to look up.
   */
  private def handleGet(key: String): Unit = {
    store.get(key) match {
      case Some(entry) if !entry.isExpired =>
        hits += 1
        store.remove(key)
        store.put(key, entry)
        sender() ! Some(entry.value)

      case Some(_) =>
        store.remove(key)
        misses += 1
        sender() ! None

      case None =>
        misses += 1
        sender() ! None
    }
  }

  /**
   * Removes all entries from the cache that have exceeded their TTL.
   * Sends back the number of entries evicted.
   */
  private def handleEvictExpired(): Unit = {
    val expired = store.filter(_._2.isExpired).keys.toList
    expired.foreach(store.remove)
    evictions += expired.size
    log.info("Evicted {} expired entries", expired.size)
    sender() ! expired.size
  }
}
