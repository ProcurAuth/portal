package app.portalinbox.portal

/** The rule of the column `last_sync_ms`: written at most once a minute, so that a busy portal does not write the database for each frame. */
object LastSync {
    const val MIN_INTERVAL_MS = 60_000L

    /**
     * True when the time [atMs] of a frame must be written. [lastWrittenMs] is the value of the last write of this run (null
     * after a start). A forced write always goes. A frame time that is before the last write (the phone clock went back) is
     * written too, so the value does not stay in the future.
     */
    fun shouldWrite(lastWrittenMs: Long?, atMs: Long, force: Boolean): Boolean =
        force || lastWrittenMs == null || atMs < lastWrittenMs || atMs - lastWrittenMs >= MIN_INTERVAL_MS
}
