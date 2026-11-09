package app.portalinbox.data

import app.portalinbox.proto.AgentDoFrame
import app.portalinbox.proto.AgentOp
import app.portalinbox.proto.AgentResultStatus
import app.portalinbox.proto.AnswerStatus
import app.portalinbox.proto.ClientFrames
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

/** The actions of the owner on an agent: the plan, the outbox, the result and the words. */
class AgentActionsTest {
    private var now = 1_800_000_000_000L
    private var msg = 0
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val a1 = Make.agentId(1)
    private val a2 = Make.agentId(2)
    private val a3 = Make.agentId(3)
    private var delivery = 0x600

    private fun hold(server: Long, id: String, file: String = "agent-known.json", edit: (JSONObject) -> Unit = {}) {
        engine.onAgent(server, Make.agent(id, Make.hex(delivery++), 1, file, edit))
    }

    private fun sent(s: Submit): AgentDoFrame {
        assertTrue("$s", s is Submit.Queued)
        return ClientFrames.decode((s as Submit.Queued).entry.frame) as AgentDoFrame
    }

    // ------------------------------------------------------------ the plan of each action

    @Test
    fun allowWithAName() {
        hold(1, a1, "agent-new.json")
        val f = sent(engine.agentDo(1, a1, AgentOp.ALLOW, name = "  Refund cleaner "))
        assertEquals(AgentOp.ALLOW, f.op)
        assertEquals("Refund cleaner", f.name)
        assertNull(f.into)
        assertEquals("m-0001", f.clientMsgId)
    }

    @Test
    fun allowWithNoNameOrABlankNameSendsNoName() {
        hold(1, a1, "agent-new.json")
        assertNull(sent(engine.agentDo(1, a1, AgentOp.ALLOW)).name)
        assertNull(sent(engine.agentDo(1, a1, AgentOp.ALLOW, name = "   ")).name)
    }

    @Test
    fun renameBlockUnblockAndMergeMakeTheirFrames() {
        hold(1, a1)
        hold(1, a2) { it.put("label", "Billing bot") }
        hold(1, a3, "agent-blocked.json")
        assertEquals("Night ETL", sent(engine.agentDo(1, a1, AgentOp.RENAME, name = "Night ETL")).name)
        assertEquals(AgentOp.BLOCK, sent(engine.agentDo(1, a1, AgentOp.BLOCK)).op)
        assertEquals(AgentOp.UNBLOCK, sent(engine.agentDo(1, a3, AgentOp.UNBLOCK)).op)
        val merge = sent(engine.agentDo(1, a1, AgentOp.MERGE, into = a2))
        assertEquals(a2, merge.into)
        assertNull(merge.name)
    }

    @Test
    fun theFrameIsSavedInTheOutboxWithTheAgentIdAndTheTypeAgentDo() {
        hold(1, a1)
        val s = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        assertEquals(OutboxType.AGENT_DO, s.entry.type)
        assertEquals(a1, s.entry.requestId)
        assertEquals(listOf(s.entry), engine.pendingOutbox(1))
        assertTrue(s.entry.isPending)
    }

    @Test
    fun anActionThatTheStateDoesNotAllowIsRefusedBeforeAnyFrame() {
        hold(1, a1)
        hold(1, a2, "agent-blocked.json")
        hold(1, a3, "agent-merged.json") { it.put("merged_into", a1) }
        hold(1, Make.agentId(4), "agent-bucket.json")
        fun refused(id: String, op: AgentOp, name: String? = null, into: String? = null) =
            (engine.agentDo(1, id, op, name, into) as? Submit.Refused)?.reason
        assertEquals(Refusal.BAD_STATE, refused(a1, AgentOp.ALLOW))
        assertEquals(Refusal.BAD_STATE, refused(a1, AgentOp.UNBLOCK))
        assertEquals(Refusal.BAD_STATE, refused(a2, AgentOp.BLOCK))
        assertEquals(Refusal.BAD_STATE, refused(a3, AgentOp.RENAME, "x"))
        assertEquals(Refusal.BAD_STATE, refused(a3, AgentOp.MERGE, into = a1))
        for (op in AgentOp.entries) assertEquals("bucket $op", Refusal.BAD_STATE, refused(Make.agentId(4), op, "x", a1))
        assertEquals(Refusal.NOT_FOUND, refused(Make.agentId(9), AgentOp.BLOCK))
        assertTrue(engine.pendingOutbox(1).isEmpty())
    }

    @Test
    fun aMergeNeedsALiveAgentOfTheSameKeyAndPortal() {
        hold(1, a1)
        hold(1, a2) { it.put("key_name", "other-key") }
        hold(1, a3, "agent-blocked.json")
        hold(2, Make.agentId(5))
        fun refused(into: String) = (engine.agentDo(1, a1, AgentOp.MERGE, into = into) as? Submit.Refused)?.reason
        assertEquals("other key", Refusal.BAD_INPUT, refused(a2))
        assertEquals("blocked target", Refusal.BAD_INPUT, refused(a3))
        assertEquals("itself", Refusal.BAD_INPUT, refused(a1))
        assertEquals("other portal", Refusal.BAD_INPUT, refused(Make.agentId(5)))
        assertEquals("not held", Refusal.BAD_INPUT, refused(Make.agentId(8)))
    }

    @Test
    fun aBadNameIsRefusedAndNeverCleaned() {
        hold(1, a1)
        val long = "x".repeat(33)
        for (bad in listOf("", "   ", long, "a\u0007b", "a‮b", "a​b", "a b")) {
            val s = engine.agentDo(1, a1, AgentOp.RENAME, name = bad)
            assertEquals("<$bad>", Refusal.BAD_INPUT, (s as Submit.Refused).reason)
        }
        assertTrue(engine.pendingOutbox(1).isEmpty())
        // 32 characters are accepted, also when they are not ASCII.
        assertEquals("é".repeat(32), sent(engine.agentDo(1, a1, AgentOp.RENAME, name = "é".repeat(32))).name)
    }

    @Test
    fun theNameCheckSaysWhyInPlainWords() {
        assertEquals("Write a name first.", AgentActions.nameLine(AgentActions.checkName("  ")))
        assertEquals("Use 32 characters at most.", AgentActions.nameLine(AgentActions.checkName("y".repeat(40))))
        assertEquals("The name has a character that is not allowed.", AgentActions.nameLine(AgentActions.checkName("a‮b")))
        assertNull(AgentActions.nameLine(AgentActions.checkName("Refund cleaner")))
    }

    @Test
    fun theSameActionThatAlreadyWaitsIsNotQueuedTwice() {
        hold(1, a1)
        val first = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        val second = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        assertEquals(first.entry.clientMsgId, second.entry.clientMsgId)
        assertEquals(1, engine.pendingOutbox(1).size)
        // Another name is another action.
        engine.agentDo(1, a1, AgentOp.RENAME, name = "A")
        engine.agentDo(1, a1, AgentOp.RENAME, name = "B")
        assertEquals(3, engine.pendingOutbox(1).size)
    }

    // ------------------------------------------------------------ what each state allows

    @Test
    fun theActionsOfEachState() {
        hold(1, a1, "agent-new.json")
        hold(1, a2)
        hold(1, a3, "agent-blocked.json")
        hold(1, Make.agentId(4), "agent-merged.json") { it.put("merged_into", a2) }
        hold(1, Make.agentId(5), "agent-bucket.json")
        val d = AgentDirectory(repo.agents(1))
        fun ops(id: String) = d.get(1, id)!!.let { AgentActions.available(it, d.mergeTargets(it)) }
        assertEquals(setOf(AgentOp.ALLOW, AgentOp.RENAME, AgentOp.BLOCK, AgentOp.MERGE), ops(a1))
        assertEquals(setOf(AgentOp.RENAME, AgentOp.BLOCK, AgentOp.MERGE), ops(a2))
        assertEquals(setOf(AgentOp.UNBLOCK, AgentOp.RENAME), ops(a3))
        assertTrue(ops(Make.agentId(4)).isEmpty())
        assertTrue(ops(Make.agentId(5)).isEmpty())
    }

    @Test
    fun withNoOtherAgentOfTheKeyThereIsNoMerge() {
        hold(1, a1)
        val d = AgentDirectory(repo.agents(1))
        assertFalse(AgentOp.MERGE in AgentActions.available(d.get(1, a1)!!, d.mergeTargets(d.get(1, a1)!!)))
    }

    // ------------------------------------------------------------ the result

    @Test
    fun theResultClosesTheEntryAndGivesTheEventWithTheLine() {
        hold(1, a1, "agent-new.json")
        val queued = engine.agentDo(1, a1, AgentOp.ALLOW, name = "Refund cleaner") as Submit.Queued
        val out = engine.onAgentResult(1, Make.agentResult(queued.entry.clientMsgId, a1))
        assertNull(out.ack)
        val ev = out.events.single() as ItemEvent.AgentResult
        assertEquals(queued.entry.clientMsgId, ev.entry!!.clientMsgId)
        assertEquals("The agent is allowed. You can approve its requests now.", AgentActions.resultLine(ev.result, ev.entry))
        assertTrue(engine.pendingOutbox(1).isEmpty())
        assertEquals("ok", repo.outbox(1, queued.entry.clientMsgId)!!.result)
    }

    @Test
    fun aResultForNoEntryOrAnEntryThatIsNotAnAgentActionChangesNothing() {
        hold(1, a1)
        assertTrue(engine.onAgentResult(1, Make.agentResult("m-zzz", a1)).events.isEmpty())
        // An answer entry with this id is not closed by an agent result.
        repo.putOutbox(OutboxEntry(1, "m-ans", Make.hex(5), OutboxType.ANSWER, "{}", now, 0, null, null, null))
        assertTrue(engine.onAgentResult(1, Make.agentResult("m-ans", a1)).events.isEmpty())
        assertTrue(repo.outbox(1, "m-ans")!!.isPending)
        // A result twice: the second one has no entry that waits.
        val q = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        assertEquals(1, engine.onAgentResult(1, Make.agentResult(q.entry.clientMsgId, a1)).events.size)
        assertTrue(engine.onAgentResult(1, Make.agentResult(q.entry.clientMsgId, a1)).events.isEmpty())
    }

    @Test
    fun everyStatusOfTheClosedListHasItsOwnLine() {
        val lines = AgentResultStatus.entries.map { AgentActions.resultLine(it, AgentOp.BLOCK) }
        assertEquals(AgentResultStatus.entries.size, lines.toSet().size)
        assertEquals("The name is saved.", AgentActions.resultLine(AgentResultStatus.OK, AgentOp.RENAME))
        assertEquals("The agents are merged.", AgentActions.resultLine(AgentResultStatus.OK, AgentOp.MERGE))
        assertEquals("Another agent of this key has this name. Use another name.", AgentActions.resultLine(AgentResultStatus.NAME_TAKEN, null))
        assertEquals("An agent can be merged only into an agent of the same key.", AgentActions.resultLine(AgentResultStatus.CROSS_KEY, AgentOp.MERGE))
        assertEquals("This portal does not use agents.", AgentActions.resultLine(AgentResultStatus.NOT_ENABLED, AgentOp.ALLOW))
        assertTrue(AgentResultStatus.entries.all { (it == AgentResultStatus.OK) == AgentActions.succeeded(it) })
    }

    @Test
    fun theLinesAreAsciiAndNeverNameAnAgent() {
        for (s in AgentResultStatus.entries) for (op in AgentOp.entries + listOf(null)) {
            val line = AgentActions.resultLine(s, op)
            assertTrue(line, line.all { it.code in 0x20..0x7E })
            assertFalse(line, line.contains("server", ignoreCase = true))
        }
    }

    // ------------------------------------------------------------ the outbox stays apart from the answers

    @Test
    fun anAgentActionIsNeverAnAnswerOfAnItem() {
        hold(1, a1)
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "Billing bot") })
        val q = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        // The agent id is not a request id: the screen of the item sees no progress.
        assertEquals(AnswerProgress.None, OutboxAnswer.progress(repo.outboxOf(1, Make.hex(0x11))))
        // The approval can still be answered while the action waits.
        assertTrue(engine.approve(1, Make.hex(0x11)) is Submit.Queued)
        assertEquals(setOf(AgentOp.BLOCK), AgentActions.pendingOps(engine.pendingOutbox(1), a1))
        assertEquals(AgentOp.BLOCK, AgentActions.frameOf(q.entry)!!.op)
        assertNull(AgentActions.frameOf(engine.pendingOutbox(1).first { it.type == OutboxType.ANSWER }))
    }

    @Test
    fun theOutboxFrameGoesOutAgainAfterTheNextWelcome() {
        hold(1, a1)
        val q = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        val start = engine.onWelcome(1, Make.welcome())
        assertNotNull(start.sync)
        assertEquals(listOf(q.entry.clientMsgId), start.outbox.map { it.clientMsgId })
        assertTrue(start.outbox.single().toFrame() is AgentDoFrame)
    }

    @Test
    fun theLockedAnswerKeepsTheItemOpenWithAClearText() {
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "claude-laptop #4", state = "new") })
        val q = engine.approve(1, Make.hex(0x11)) as Submit.Queued
        engine.onAnswerResult(1, Make.result(Make.hex(0x11), q.entry.clientMsgId, "agent_locked"))
        val item = repo.item(1, Make.hex(0x11))!!
        assertTrue(item.isOpen)
        val done = OutboxAnswer.progress(repo.outboxOf(1, Make.hex(0x11))) as AnswerProgress.Done
        assertEquals(AnswerStatus.AGENT_LOCKED, done.status)
        assertEquals("This agent is new and locked. Allow the agent first, then approve.", OutboxAnswer.resultLine(done))
        assertTrue(OutboxAnswer.canTryAgain(done))
    }

    @Test
    fun aClosedActionLeavesTheOutboxAfterAWeekAndAnOpenOneNever() {
        hold(1, a1)
        val done = engine.agentDo(1, a1, AgentOp.BLOCK) as Submit.Queued
        engine.onAgentResult(1, Make.agentResult(done.entry.clientMsgId, a1))
        val waiting = engine.agentDo(1, a1, AgentOp.RENAME, name = "x") as Submit.Queued
        now += ItemEngine.AGENT_ACTION_KEEP_MS - 1
        engine.onWelcome(1, Make.welcome())
        assertNotNull(repo.outbox(1, done.entry.clientMsgId))
        now += 2
        engine.onWelcome(1, Make.welcome())
        assertNull(repo.outbox(1, done.entry.clientMsgId))
        assertTrue(repo.outbox(1, waiting.entry.clientMsgId)!!.isPending)
    }
}
