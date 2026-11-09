package app.portalinbox.alert

/**
 * Runs the one notification: computes the aggregate, decides, plans, and gives the result to the [Sink]. Pure Kotlin. The Sink
 * is the only place that touches Android, so a JVM test can prove that nothing but the one id and the one tag is ever used.
 */
class AttentionCenter(private val sink: Sink) {
    interface Sink {
        /** The notification is on screen now. */
        fun isShown(): Boolean

        /** Posts or updates the notification. The id and the tag are [Attention.ID] and [Attention.TAG], always. */
        fun post(plan: Attention.Plan)

        /** Removes the notification. */
        fun cancel()

        /** Plays [sound]: [Attention.Sound.PHONE] or [Attention.Sound.RING]. The alarm has its own player. */
        fun play(sound: Attention.Sound)
    }

    private var shown: Map<String, Attention.Rank>? = null
    private var lastKey: String? = null

    /**
     * Updates the notification for the state in [input]. [force]: an event happened (an item, an alert, a digest, a reply, a
     * mark as read), so the ages and the counts are posted again even if the shape did not change. A tick passes false: it
     * posts only when the shape changed, so a refresh never brings back a notification that the owner removed.
     * [alarmDirect]: the alarm page is open or opens by itself.
     * Returns the plan that was posted, or null.
     */
    @Synchronized
    fun update(input: Attention.Input, force: Boolean = true, alarmDirect: Boolean = false): Attention.Plan? {
        val agg = Attention.compute(input)
        val decision = Attention.decide(shown, agg)
        shown = agg.entries.associate { it.key to it.rank }
        val key = agg.key
        if (agg.isEmpty) {
            if (force || lastKey != "") sink.cancel()
            lastKey = ""
            return null
        }
        if (!force && key == lastKey && !decision.alert) return null
        val ringing = agg.ringing.isNotEmpty()
        val onScreen = sink.isShown()
        lastKey = key
        // The owner removed the notification and nothing new came: it stays removed. The list holds everything.
        if (!decision.arrivals && !decision.alert && !ringing && !onScreen) return null
        val plan = Attention.plan(agg, decision, alarmDirect, onScreen)
        sink.post(plan)
        if (decision.alert && (decision.sound == Attention.Sound.PHONE || decision.sound == Attention.Sound.RING)) sink.play(decision.sound)
        return plan
    }
}
