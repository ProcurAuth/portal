package app.portalinbox.ui

/**
 * The gate "read to the end" of the command. Approve stays locked until the owner scrolled the
 * command box to its end. Pure Kotlin: the screen gives the numbers of the scroll box.
 *
 * Rules:
 * - The gate belongs to one command, named by [State.key] (the effect hash of the command). A change of the
 *   command resets the gate: what the owner read is not what is on the screen.
 * - A command that fits in the box is read at once.
 * - Once read, the gate stays open for this key. Scrolling back up never locks it again.
 * - [TOLERANCE_PX] absorbs the rounding of the scroll position.
 */
object ReadGate {
    const val TOLERANCE_PX = 2

    data class State(val key: String, val readToEnd: Boolean, val progressPercent: Int) {
        val locked: Boolean get() = !readToEnd
    }

    fun start(key: String): State = State(key, readToEnd = false, progressPercent = 0)

    /**
     * The state after the scroll box changed. [scrollY] is the scroll offset, [viewport] the height of the box
     * and [content] the height of the text inside it, all in pixels. A box that is not measured yet
     * (`viewport` or `content` not above zero) changes nothing.
     */
    fun observe(state: State, key: String, scrollY: Int, viewport: Int, content: Int): State {
        if (key != state.key) return observe(start(key), key, scrollY, viewport, content)
        if (viewport <= 0 || content <= 0) return state
        val bottom = scrollY.coerceAtLeast(0) + viewport
        val atEnd = bottom >= content - TOLERANCE_PX
        val percent = if (atEnd) 100 else (bottom * 100L / content).toInt().coerceIn(0, 99)
        if (state.readToEnd) return state
        return State(key, atEnd, percent)
    }

    /** The state that a screen saved, or a fresh gate when the saved key is not the key of the command. */
    fun restore(key: String, savedKey: String?, savedRead: Boolean): State =
        if (savedKey == key && savedRead) State(key, true, 100) else start(key)
}
