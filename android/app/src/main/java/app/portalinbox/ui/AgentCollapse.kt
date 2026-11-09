package app.portalinbox.ui

/**
 * Which groups of the list "To answer" the owner folded. The state is local to the phone: the portal never knows it. Pure
 * Kotlin and immutable. The key of a group is the key of [app.portalinbox.data.AgentKey]: the portal and the agent.
 * The text form goes into the preferences of the app.
 */
data class AgentCollapse(val folded: Set<String> = emptySet()) {
    fun isFolded(key: String): Boolean = key in folded

    /** A tap on the header of a group: folds it, or opens it again. */
    fun toggle(key: String): AgentCollapse = AgentCollapse(if (key in folded) folded - key else folded + key)

    /** Forgets the groups that no longer exist, so the set never grows. */
    fun retain(live: Collection<String>): AgentCollapse {
        val alive = live.toSet()
        val keep = folded.filter { it in alive }.toSet()
        return if (keep.size == folded.size) this else AgentCollapse(keep)
    }

    /** The text for the preferences: the keys, one after the other, with a comma between. A key has no comma. */
    fun encode(): String = folded.sorted().joinToString(",")

    companion object {
        /** The most keys that the phone keeps. A text with more is cut: the extra groups open. */
        const val MAX_KEYS = 400

        fun decode(text: String?): AgentCollapse {
            if (text.isNullOrEmpty()) return AgentCollapse()
            val keys = text.split(',').filter { isKey(it) }.take(MAX_KEYS).toSet()
            return AgentCollapse(keys)
        }

        /** A key is `<digits>:<32 hexadecimal characters>`. Any other text from the preferences is not a key. */
        fun isKey(text: String): Boolean {
            val at = text.indexOf(':')
            if (at < 1 || !text.substring(0, at).all { it in '0'..'9' }) return false
            val id = text.substring(at + 1)
            return id.length == 32 && id.all { it in '0'..'9' || it in 'a'..'f' }
        }
    }
}
