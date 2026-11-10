package app.portalinbox.ui

import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.AnswerProgress
import app.portalinbox.proto.AgentOp
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.Observed
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId

/** The box "From agent" of the approval screen, the agent screen, the marking of the text of a client and the lock of Approve. */
class AgentBoxesTest {
    private val zone = ZoneId.of("UTC")
    private val t0 = 1_788_000_000_000L
    private var now = t0 + 3_600_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val billing = Make.agentId(1)
    private val fresh = Make.agentId(3)
    private var n = 0

    private fun hold(id: String, file: String = "agent-known.json", rev: Long = 1, server: Long = 1, edit: (JSONObject) -> Unit = {}) {
        engine.onAgent(server, Make.agent(id, Make.hex(0x900 + n++), rev, file, edit))
    }

    private fun ask(agent: String, label: String, state: String = "known", says: String? = "billing-bot", edit: (JSONObject) -> Unit = {}): Item {
        val rid = Make.hex(0x100 + n++)
        engine.onRequest(1, Make.approval(rid, Make.hex(0x300 + n++), createdAtMs = now - 60_000) {
            Make.asAgent(it, agent, label, state)
            if (says == null) it.remove("agent_label") else it.put("agent_label", says)
            edit(it)
        })
        return repo.item(1, rid)!!
    }

    private fun dir() = AgentDirectory(repo.allAgents())

    // ------------------------------------------------------------ the box From agent

    @Test
    fun theBoxHoldsTheNameTheStateTheKeyAndWhatTheAgentSays() {
        hold(billing) { it.put("label", "Billing bot"); it.getJSONObject("observed").put("says", "billing-bot") }
        val b = FromAgentBox.of(ask(billing, "x"), dir())
        assertEquals("Billing bot", b.name)
        assertEquals("KNOWN", b.stateWord)
        assertEquals("claude-laptop", b.keyName)
        assertEquals("billing-bot", b.says)
        assertNull(b.saysHere)
        assertFalse(b.locked)
        assertFalse(b.isNew)
        assertTrue(b.canOpenAgent)
    }

    @Test
    fun theStateComesFromTheTableSoAnAllowedAgentIsNotNewAnyMore() {
        hold(fresh, "agent-new.json")
        val item = ask(fresh, "claude-laptop #4", state = "new")
        assertTrue(FromAgentBox.of(item, dir()).isNew)
        assertTrue(FromAgentBox.of(item, dir()).locked)
        hold(fresh, "agent-known.json", rev = 9)
        val after = FromAgentBox.of(repo.item(1, item.requestId)!!, dir())
        assertFalse(after.isNew)
        assertFalse(after.locked)
        assertEquals("KNOWN", after.stateWord)
    }

    @Test
    fun withTheSwitchOffTheNewAgentIsNotLocked() {
        hold(fresh, "agent-new.json") { it.put("lock_accept", false) }
        val b = FromAgentBox.of(ask(fresh, "claude-laptop #4", state = "new"), dir())
        assertTrue(b.isNew)
        assertFalse(b.locked)
    }

    @Test
    fun anAgentThatThePhoneDoesNotHoldFallsBackToTheFrameAndIsNotLocked() {
        val b = FromAgentBox.of(ask(fresh, "Name of the frame", state = "new"), dir())
        assertEquals("Name of the frame", b.name)
        assertTrue(b.isNew)
        assertFalse(b.locked)
        assertFalse(b.canOpenAgent)
        assertEquals("billing-bot", b.says)
    }

    @Test
    fun aRequestThatSaysAnotherNameThanTheTableIsShownToo() {
        hold(billing) { it.getJSONObject("observed").put("says", "billing-bot") }
        val b = FromAgentBox.of(ask(billing, "x", says = "payments-admin"), dir())
        assertEquals("billing-bot", b.says)
        assertEquals("payments-admin", b.saysHere)
    }

    @Test
    fun theBucketHasNoSaysAndNoState() {
        hold(Make.agentId(4), "agent-bucket.json")
        val b = FromAgentBox.of(ask(Make.agentId(4), "Unattributed", state = "known", says = "evil") { it.getJSONObject("from").put("agent_kind", "bucket") }, dir())
        assertTrue(b.isBucket)
        assertNull(b.says)
        assertEquals("NOT KNOWN", b.stateWord)
    }

    @Test
    fun aNewAgentThatSaysTheNameOfAKnownAgentGetsTheWarning() {
        hold(billing) { it.put("label", "Billing bot") }
        hold(fresh, "agent-new.json") { it.getJSONObject("observed").put("says", "Billing Bot").remove("says_changed"); it.remove("similar") }
        val b = FromAgentBox.of(ask(fresh, "claude-laptop #4", state = "new"), dir())
        assertEquals(listOf("Same text as your agent Billing bot.", AgentTexts.COPY_NOTE), b.warnings)
    }

    // ------------------------------------------------------------ the lock of Approve

    private fun screen(box: FromAgentBox.Box, item: Item) = ApprovalState.of(
        ApprovalState.Input(item, now, ReadGate.State("k", true, 100), false, AnswerProgress.None, true, true, agentLocked = box.locked),
    )

    @Test
    fun approveIsLockedForANewAgentAndDeclineStaysOn() {
        hold(fresh, "agent-new.json")
        val item = ask(fresh, "claude-laptop #4", state = "new")
        val s = screen(FromAgentBox.of(item, dir()), item)
        assertFalse(s.approveEnabled)
        assertEquals(ApprovalState.Lock.AGENT_LOCKED, s.approveLock)
        assertEquals(AgentTexts.LOCK_HINT, s.hint)
        assertTrue(s.declineEnabled)
        assertEquals(ApprovalState.Mode.OPEN, s.mode)
    }

    @Test
    fun approveWorksAgainWhenTheAgentIsAllowed() {
        hold(fresh, "agent-new.json")
        val item = ask(fresh, "claude-laptop #4", state = "new")
        hold(fresh, "agent-known.json", rev = 4)
        val s = screen(FromAgentBox.of(item, dir()), item)
        assertTrue(s.approveEnabled)
        assertNull(s.approveLock)
    }

    @Test
    fun theLockOfTheAgentCountsBeforeTheReadGate() {
        hold(fresh, "agent-new.json")
        val item = ask(fresh, "claude-laptop #4", state = "new")
        val s = ApprovalState.of(ApprovalState.Input(item, now, ReadGate.start("k"), false, AnswerProgress.None, true, true, agentLocked = true))
        assertEquals(ApprovalState.Lock.AGENT_LOCKED, s.approveLock)
    }

    // ------------------------------------------------------------ the marking of the text of a client

    @Test
    fun aLookAlikeLetterAndAHiddenCharacterShowAsMarks() {
        // A Cyrillic o and a no-break space: neither is an ASCII letter.
        val marked = UntrustedText.plain("billing bоt ")
        assertTrue(marked, marked.contains("U+043E"))
        assertTrue(marked, marked.contains("<U+00A0>"))
        assertTrue(UntrustedText.hasMarks("bоt"))
        assertFalse(UntrustedText.hasMarks("billing-bot"))
    }

    @Test
    fun theQuotedTextKeepsItsSpansInPlace() {
        val q = UntrustedText.quoted("bоt")
        assertTrue(q.text.startsWith("\"") && q.text.endsWith("\""))
        assertTrue(q.spans.isNotEmpty())
        for (s in q.spans) assertTrue(s.start >= 1 && s.end <= q.text.length - 1)
    }

    @Test
    fun theObservedRowsOfTheAgentScreenHaveTheDetails() {
        val o = Observed("x", 2, "claude-code/2.1.0", "claude-code/2.1.0 (linux; x64)", "203.0.113.0/24", 3)
        val rows = AgentTexts.observedRows(o, detail = true).associate { it.label to it.value }
        assertEquals("x (changed 2 times)", rows["Says it is"])
        assertEquals("claude-code/2.1.0 (linux; x64)", rows["Program"])
        assertEquals("203.0.113.0/24 (3 ranges seen)", rows["Address"])
        assertNull(AgentTexts.observedRows(o).firstOrNull { it.label == "Program" })
        assertTrue(AgentTexts.observedRows(null).isEmpty())
    }

    // ------------------------------------------------------------ the agent screen

    @Test
    fun theAgentScreenCountsOpenAndAnsweredItemsItself() {
        hold(billing) { it.put("label", "Billing bot"); it.put("open", 99) }
        val a = ask(billing, "x")
        ask(billing, "x")
        val done = ask(billing, "x")
        engine.onSettled(1, Make.settled(done.requestId, Make.hex(0xE00)))
        val agent = dir().get(1, billing)!!
        val m = AgentScreenModel.of(agent, dir(), repo.items(1), now, zone, emptySet())
        assertEquals("Billing bot", m.name)
        assertEquals("Agent of key claude-laptop", m.keyLine)
        assertTrue(m.factsLine, m.factsLine.endsWith("2 open · 1 answered"))
        assertEquals(2, m.open.size)
        assertEquals("KNOWN", m.stateWord)
        assertNotNull(m.observed.firstOrNull { it.label == "Client" })
        assertEquals(a.requestId, m.open.first().requestId)
    }

    @Test
    fun theAgentScreenOffersTheActionsOfTheStateWithoutThePendingOnes() {
        hold(billing)
        hold(Make.agentId(2)) { it.put("label", "Other") }
        val agent = dir().get(1, billing)!!
        val all = AgentScreenModel.of(agent, dir(), emptyList(), now, zone, emptySet())
        assertEquals(setOf(AgentOp.RENAME, AgentOp.BLOCK, AgentOp.MERGE), all.actions)
        val pending = AgentScreenModel.of(agent, dir(), emptyList(), now, zone, setOf(AgentOp.BLOCK))
        assertEquals(setOf(AgentOp.RENAME, AgentOp.MERGE), pending.actions)
        assertEquals(setOf(AgentOp.BLOCK), pending.pending)
        assertEquals(listOf("Other"), all.mergeTargets.map { it.label })
    }

    @Test
    fun aBlockedAgentCanBeUnblockedAndTheBucketHasNoActionAndNoHints() {
        hold(billing, "agent-blocked.json")
        hold(Make.agentId(4), "agent-bucket.json")
        val blocked = AgentScreenModel.of(dir().get(1, billing)!!, dir(), emptyList(), now, zone, emptySet())
        assertTrue(AgentOp.UNBLOCK in blocked.actions)
        assertEquals("BLOCKED", blocked.stateWord)
        val bucket = AgentScreenModel.of(dir().get(1, Make.agentId(4))!!, dir(), emptyList(), now, zone, emptySet())
        assertTrue(bucket.actions.isEmpty())
        assertTrue(bucket.observed.isEmpty())
        assertTrue(bucket.isBucket)
        assertEquals("NOT KNOWN", bucket.stateWord)
    }

    @Test
    fun aMergedAgentNamesItsTargetAndHasNoAction() {
        hold(billing) { it.put("label", "Billing bot") }
        hold(Make.agentId(2), "agent-merged.json") { it.put("merged_into", billing) }
        val m = AgentScreenModel.of(dir().get(1, Make.agentId(2))!!, dir(), emptyList(), now, zone, emptySet())
        assertEquals("This agent was merged into Billing bot.", m.mergedNote)
        assertTrue(m.actions.isEmpty())
        assertEquals(AgentState.MERGED, m.agent.state)
    }

    @Test
    fun theWordsOfTheStatesAreOnePerState() {
        val words = AgentState.entries.map { AgentTexts.stateWord(it, app.portalinbox.proto.AgentKind.AGENT) }
        assertEquals(4, words.toSet().size)
        assertEquals("NOT KNOWN", AgentTexts.stateWord(AgentState.KNOWN, app.portalinbox.proto.AgentKind.BUCKET))
    }

    @Test
    fun theFixedSentencesAreAscii() {
        for (s in listOf(AgentTexts.NO_PROOF, AgentTexts.LOCK_LINE, AgentTexts.LOCK_HINT, AgentTexts.COPY_NOTE, AgentTexts.BUCKET_HINT)) {
            assertTrue(s, s.all { it.code in 0x20..0x7E })
            assertFalse(s, s.contains("server", ignoreCase = true))
        }
    }

    @Test
    fun theSeparatorOfTheObservedLineIsNotMarked() {
        val joined = UntrustedText.joined(listOf("says \"bot\"", "client/1", "10.0.0.0/24"), " \u00B7 ")
        assertEquals("says \"bot\" \u00B7 client/1 \u00B7 10.0.0.0/24", joined.text)
        assertTrue(joined.spans.isEmpty())
        // A mark of a part stays on its own characters.
        val marked = UntrustedText.joined(listOf("a", "b\u200Bc"), " \u00B7 ")
        assertEquals("a \u00B7 b<U+200B>c", marked.text)
        assertTrue(marked.spans.all { it.start >= 4 })
    }
}
