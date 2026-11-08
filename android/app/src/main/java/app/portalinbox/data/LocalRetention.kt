package app.portalinbox.data

/**
 * How long the phone keeps the local copy of a final item (CONF 7.3: the local copy is the independent proof of what the
 * owner saw, and it is erased at `local_retention`). Pure Kotlin: the clock is an argument.
 *
 * - An open item is never erased.
 * - A final item is erased when its end is older than the retention. The end is the time of the `settled` frame, or the last
 *   change of the item.
 * - The erase removes the item, its turns and its outbox rows. A request that arrives again later is a new item.
 */
object LocalRetention {
    const val DEFAULT_DAYS = 30
    val OPTIONS_DAYS = listOf(7, 30, 90, 180, 365)
    private const val DAY_MS = 86_400_000L

    fun isOption(days: Int): Boolean = days in OPTIONS_DAYS

    /** The retention of a stored value. A value that is not an option gives the default: the phone keeps what it can read. */
    fun daysOrDefault(stored: Int?): Int = if (stored != null && isOption(stored)) stored else DEFAULT_DAYS

    fun label(days: Int): String = if (days >= 365) "1 year" else if (days >= 180) "6 months" else "$days days"

    fun cutoffMs(nowMs: Long, days: Int): Long = nowMs - days * DAY_MS

    /** The time when a final item ended, for the retention. */
    fun endOf(item: Item): Long = item.settled?.atMs ?: item.updatedAtMs

    fun expired(item: Item, nowMs: Long, days: Int): Boolean = !item.isOpen && endOf(item) < cutoffMs(nowMs, days)

    /** Erases the expired items of a server. Gives the number of items erased. */
    fun purge(repo: ItemRepository, serverId: Long, nowMs: Long, days: Int): Int = repo.transaction {
        val gone = repo.items(serverId).filter { expired(it, nowMs, days) }
        for (item in gone) repo.deleteItem(serverId, item.requestId)
        repo.deleteDeliveriesBefore(serverId, cutoffMs(nowMs, days))
        gone.size
    }
}
