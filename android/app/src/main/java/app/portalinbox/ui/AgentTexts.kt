package app.portalinbox.ui

import app.portalinbox.data.Agent
import app.portalinbox.data.AgentDirectory
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.Observed
import app.portalinbox.portal.Confusables
import java.text.Normalizer
import java.util.Locale

/**
 * The fixed words of the agent screens and the warnings about a text that an agent sent. Pure Kotlin, English, no Context.
 * The labels of buttons and titles are in `strings_agents.xml`. The sentences that depend on data are here, so a JVM test reads them.
 */
object AgentTexts {
    /** The sentence under every box of hints. The text of a client is never proof. */
    const val NO_PROOF = "Anyone with the key can send these texts. They are no proof."

    /** The line under a row of a new agent that the server locks. */
    const val LOCK_LINE = "Approve is locked until you allow this agent."

    /** The line in the bottom bar of the approval screen for a new agent that the server locks. */
    const val LOCK_HINT = "Approve is locked: this agent is new. Name it or allow it first. You can decline now."

    /** The copy warning that closes the list of the warnings of a new agent. */
    const val COPY_NOTE = "It can be that agent or a copy. The portal cannot tell."

    /** The hint of the bucket: the items that no agent claims. */
    const val BUCKET_HINT = "sent without the portal channel"

    /** The word of the state chip. */
    fun stateWord(state: AgentState, kind: AgentKind): String = when {
        kind == AgentKind.BUCKET -> "NOT KNOWN"
        state == AgentState.NEW -> "NEW"
        state == AgentState.KNOWN -> "KNOWN"
        state == AgentState.BLOCKED -> "BLOCKED"
        else -> "MERGED"
    }

    /** The state for a screen reader. */
    fun stateSpoken(state: AgentState, kind: AgentKind): String = when {
        kind == AgentKind.BUCKET -> "agent not known"
        state == AgentState.NEW -> "new agent, not allowed yet"
        state == AgentState.KNOWN -> "known agent"
        state == AgentState.BLOCKED -> "blocked agent"
        else -> "merged agent"
    }

    // ------------------------------------------------------------ same text as a name

    /** The fold of the specification (3.8.15): NFKC and lower case. Two texts that fold to the same text are the same text. */
    fun fold(text: String): String = Normalizer.normalize(text, Normalizer.Form.NFKC).lowercase(Locale.ROOT)

    enum class Match { SAME, LOOKS_LIKE }

    /** An agent of the same key whose name the text of a client equals or looks like. */
    data class NameMatch(val agent: Agent, val kind: Match)

    /**
     * The known agents of the key of [of] whose name equals [says] after the fold of 3.8.15 ([Match.SAME]), or looks like it after
     * the folding of look-alike characters ([Match.LOOKS_LIKE]). The agent [of] itself is not compared. A bucket, a blocked
     * agent and a merged agent are not compared: only the agents that the owner can still mean.
     */
    fun nameMatches(says: String?, of: Agent, dir: AgentDirectory): List<NameMatch> {
        if (says.isNullOrBlank()) return emptyList()
        val exact = fold(says.trim())
        val skel = Confusables.skeleton(says.trim())
        return dir.all.filter { it.serverId == of.serverId && it.keyName == of.keyName && it.agentId != of.agentId && it.state == AgentState.KNOWN && !it.isBucket }
            .mapNotNull { other ->
                when {
                    fold(other.label) == exact -> NameMatch(other, Match.SAME)
                    Confusables.skeleton(other.label) == skel -> NameMatch(other, Match.LOOKS_LIKE)
                    else -> null
                }
            }
            .sortedWith(compareBy({ it.kind }, { it.agent.label.lowercase() }, { it.agent.agentId }))
    }

    /**
     * The warnings for a new agent: one line for each agent that its text equals or looks like, the hint `similar` of the portal
     * when it is not already named, and one closing sentence. Empty when there is nothing to warn about. The names are text of
     * the portal. The text of the client is never copied into a warning.
     */
    fun warnings(agent: Agent, dir: AgentDirectory): List<String> {
        val out = ArrayList<String>()
        val named = LinkedHashSet<String>()
        for (m in nameMatches(agent.observed?.says, agent, dir)) {
            named += m.agent.agentId
            val name = VisibleText.escapeLine(m.agent.label)
            out += if (m.kind == Match.SAME) "Same text as your agent $name." else "Almost the same text as your agent $name, with look-alike letters."
        }
        agent.similar?.let { s ->
            if (s.agentId !in named) out += "The portal thinks it looks like your agent ${VisibleText.escapeLine(s.label)}."
        }
        if (out.isNotEmpty()) out += COPY_NOTE
        return out
    }

    // ------------------------------------------------------------ the observed box

    /** One row of the box "Observed, not verified": the label is ours, the value is a text of a client. */
    data class ObservedRow(val label: String, val value: String)

    /**
     * The rows of the observed box: what it says, its client, its address range and, for the agent screen, its program text. A
     * hint that the portal did not record has no row. [detail] adds the user agent and the counts. The value is untrusted: the
     * screen marks it with [UntrustedText].
     */
    fun observedRows(o: Observed?, detail: Boolean = false): List<ObservedRow> {
        if (o == null) return emptyList()
        val rows = ArrayList<ObservedRow>()
        o.says?.let { rows += ObservedRow("Says it is", it + (o.saysChanged?.let { n -> " (changed " + (if (n == 1L) "1 time" else "$n times") + ")" } ?: "")) }
        o.client?.let { rows += ObservedRow("Client", it) }
        if (detail) o.userAgent?.let { rows += ObservedRow("Program", it) }
        o.net?.let { rows += ObservedRow("Address", it + if (detail && o.netsSeen > 1) " (${o.netsSeen} ranges seen)" else "") }
        return rows
    }

    /** The one-line version under the header of a group: `says "billing-bot" · claude-code 2.1 · 203.0.113.0/24`. */
    fun observedParts(o: Observed?): List<String> {
        if (o == null) return emptyList()
        return listOfNotNull(o.says?.let { "says \"$it\"" }, o.client, o.net)
    }
}
