package app.portalinbox.data

import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.MAX_SYNC_AGENTS
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The table of the agents: kept by `rev`, by portal and agent, with the merge chains, the label lookup and the list of `sync`. */
class AgentTest {
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-1" })
    private val a1 = Make.agentId(1)
    private val a2 = Make.agentId(2)
    private val a3 = Make.agentId(3)
    private var delivery = 0x500

    private fun frame(agentId: String, rev: Long, file: String = "agent-known.json", edit: (org.json.JSONObject) -> Unit = {}) =
        Make.agent(agentId, Make.hex(delivery++), rev, file, edit)

    private fun put(server: Long, agentId: String, rev: Long, file: String = "agent-known.json", edit: (org.json.JSONObject) -> Unit = {}) =
        engine.onAgent(server, frame(agentId, rev, file, edit))

    private fun dir(server: Long? = null) = AgentDirectory(if (server == null) repo.allAgents() else repo.agents(server))

    // ------------------------------------------------------------ the rule of rev

    @Test
    fun anAgentFrameIsKeptAndAcknowledged() {
        val f = frame(a1, 5) { it.put("label", "Billing bot") }
        val out = engine.onAgent(1, f)
        assertEquals(f.id, out.ack)
        val a = repo.agent(1, a1)!!
        assertEquals(5L, a.rev)
        assertEquals("Billing bot", a.label)
        assertEquals(AgentState.KNOWN, a.state)
        assertFalse(a.notified)
    }

    @Test
    fun aLowerRevIsIgnoredButStillAcknowledged() {
        put(1, a1, 5) { it.put("label", "Billing bot") }
        val old = frame(a1, 4) { it.put("label", "Old name") }
        val out = engine.onAgent(1, old)
        assertEquals(old.id, out.ack)
        assertTrue(out.events.isEmpty())
        assertEquals("Billing bot", repo.agent(1, a1)!!.label)
        assertEquals(5L, repo.agent(1, a1)!!.rev)
    }

    @Test
    fun aHigherRevReplacesTheRowAndKeepsTheNotifiedFlag() {
        put(1, a1, 1, "agent-new.json")
        repo.markAgentNotified(1, a1)
        val out = put(1, a1, 2) { it.put("label", "Night ETL").put("named", true) }
        val a = repo.agent(1, a1)!!
        assertEquals(2L, a.rev)
        assertEquals(AgentState.KNOWN, a.state)
        assertEquals("Night ETL", a.label)
        assertTrue(a.notified)
        assertEquals(listOf<ItemEvent>(ItemEvent.AgentChanged(a)), out.events)
    }

    @Test
    fun theSameRevRenewsTheSnapshotOfOpenAndLastCallWithNoEvent() {
        put(1, a1, 3) { it.put("open", 1).put("last_seen_ms", 1_788_000_000_000L) }
        val out = put(1, a1, 3) { it.put("open", 4).put("last_seen_ms", 1_788_000_900_000L) }
        assertTrue(out.events.isEmpty())
        assertEquals(4L, repo.agent(1, a1)!!.open)
        assertEquals(1_788_000_900_000L, repo.agent(1, a1)!!.lastSeenMs)
    }

    @Test
    fun theSameDeliveryTwiceChangesNothingAndIsAcknowledgedAgain() {
        val f = frame(a1, 1, "agent-new.json")
        assertEquals(1, engine.onAgent(1, f).events.size)
        val again = engine.onAgent(1, f)
        assertEquals(f.id, again.ack)
        assertTrue(again.events.isEmpty())
    }

    @Test
    fun twoPortalsHoldAgentsWithTheSameIdApart() {
        put(1, a1, 1) { it.put("label", "On portal one") }
        put(2, a1, 7) { it.put("label", "On portal two") }
        assertEquals("On portal one", repo.agent(1, a1)!!.label)
        assertEquals("On portal two", repo.agent(2, a1)!!.label)
        assertEquals(1L, repo.agent(1, a1)!!.rev)
        // A lower rev on one portal is not compared with the rev of the other portal.
        put(1, a1, 2) { it.put("label", "Portal one again") }
        assertEquals("Portal one again", repo.agent(1, a1)!!.label)
        assertEquals("On portal two", repo.agent(2, a1)!!.label)
        assertEquals(2, dir().all.size)
    }

    // ------------------------------------------------------------ the first sight of a new agent

    @Test
    fun aNewAgentThatThePhoneDidNotHoldGivesTheEventOnce() {
        val first = put(1, a1, 1, "agent-new.json")
        val held = repo.agent(1, a1)!!
        assertEquals(listOf<ItemEvent>(ItemEvent.NewAgent(held)), first.events)
        // A later frame of the same agent, with a higher rev or not, is never a new agent again.
        val again = put(1, a1, 2, "agent-new.json")
        assertTrue(again.events.none { it is ItemEvent.NewAgent })
        val same = put(1, a1, 2, "agent-new.json")
        assertTrue(same.events.none { it is ItemEvent.NewAgent })
    }

    @Test
    fun aKnownAgentOrABucketIsNotANewAgent() {
        assertTrue(put(1, a1, 1).events.none { it is ItemEvent.NewAgent })
        assertTrue(put(1, a2, 1, "agent-bucket.json").events.none { it is ItemEvent.NewAgent })
    }

    @Test
    fun theLockAndTheHintsOfTheFrameAreKept() {
        put(1, a1, 1, "agent-new.json")
        val a = repo.agent(1, a1)!!
        assertTrue(a.locksAccept)
        assertEquals("billing-bot", a.observed!!.says)
        assertEquals("Billing bot", a.similar!!.label)
        put(1, a1, 2) { it.put("lock_accept", false) }
        assertFalse(repo.agent(1, a1)!!.locksAccept)
    }

    // ------------------------------------------------------------ the merge chain

    @Test
    fun aMergedAgentFollowsMergedIntoUntilAnAgentThatIsNotMerged() {
        put(1, a3, 1)
        put(1, a2, 1, "agent-merged.json") { it.put("merged_into", a3) }
        put(1, a1, 1, "agent-merged.json") { it.put("merged_into", a2) }
        val r = dir().resolve(1, a1)
        assertEquals(a3, r.agentId)
        assertEquals(AgentState.KNOWN, r.agent!!.state)
    }

    @Test
    fun aChainWithAMissingTargetEndsAtTheTargetIdWithNoRow() {
        put(1, a1, 1, "agent-merged.json") { it.put("merged_into", a2) }
        val r = dir().resolve(1, a1)
        assertEquals(a2, r.agentId)
        assertNull(r.agent)
    }

    @Test
    fun aLoopInTheChainStopsAndNeverHangs() {
        put(1, a1, 1, "agent-merged.json") { it.put("merged_into", a2) }
        put(1, a2, 1, "agent-merged.json") { it.put("merged_into", a1) }
        val r = dir().resolve(1, a1)
        assertNotNull(r.agent)
        assertEquals(AgentState.MERGED, r.agent!!.state)
    }

    @Test
    fun aKnownAgentResolvesToItselfAndAnUnknownIdToItself() {
        put(1, a1, 1)
        assertEquals(a1, dir().resolve(1, a1).agentId)
        val r = dir().resolve(1, a3)
        assertEquals(a3, r.agentId)
        assertNull(r.agent)
        // The chain does not cross portals.
        put(2, a2, 1)
        assertNull(dir().resolve(1, a2).agent)
    }

    @Test
    fun theItemsOfAMergedAgentCountForTheTarget() {
        put(1, a2, 1) { it.put("label", "Billing bot") }
        put(1, a1, 1, "agent-merged.json") { it.put("merged_into", a2) }
        val mine = Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "claude-laptop #3") }
        val other = Make.approval(Make.hex(0x12), Make.hex(0x22)) { Make.asAgent(it, a2, "Billing bot") }
        engine.onRequest(1, mine)
        engine.onRequest(1, other)
        val d = dir()
        assertEquals(2, d.openCount(d.get(1, a2)!!, repo.openItems(1)))
    }

    // ------------------------------------------------------------ the label of an item

    @Test
    fun theLabelComesFromTheTableAndARenameShowsOnOldItemsAtOnce() {
        put(1, a1, 1) { it.put("label", "claude-laptop #3").put("named", false) }
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "claude-laptop #3") })
        val item = repo.item(1, Make.hex(0x11))!!
        assertEquals("claude-laptop #3", dir().labelOf(item))
        put(1, a1, 2) { it.put("label", "Billing bot").put("named", true) }
        // The frame of the item is not changed: the old item shows the new name.
        assertEquals("claude-laptop #3", item.request.from.label)
        assertEquals("Billing bot", dir().labelOf(repo.item(1, Make.hex(0x11))!!))
    }

    @Test
    fun theLabelOfTheFrameIsTheFallbackWhenTheAgentIsNotHeld() {
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "Name in the frame") })
        assertEquals("Name in the frame", dir().labelOf(repo.item(1, Make.hex(0x11))!!))
    }

    @Test
    fun theLabelOfAnItemOfAMergedAgentIsTheNameOfTheTarget() {
        put(1, a2, 1) { it.put("label", "Billing bot") }
        put(1, a1, 1, "agent-merged.json") { it.put("merged_into", a2) }
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "claude-laptop #3") })
        assertEquals("Billing bot", dir().labelOf(repo.item(1, Make.hex(0x11))!!))
    }

    @Test
    fun theLabelLookupUsesThePortalOfTheItem() {
        put(2, a1, 1) { it.put("label", "Other portal") }
        engine.onRequest(1, Make.approval(Make.hex(0x11), Make.hex(0x21)) { Make.asAgent(it, a1, "Mine") })
        assertEquals("Mine", dir().labelOf(repo.item(1, Make.hex(0x11))!!))
    }

    // ------------------------------------------------------------ the merge targets

    @Test
    fun theMergeTargetsAreTheLiveAgentsOfTheSameKeyAndPortalTheKnownFirst() {
        put(1, a1, 1, "agent-new.json")
        put(1, a2, 1) { it.put("label", "Zed") }
        put(1, a3, 1) { it.put("label", "Alpha") }
        put(1, Make.agentId(4), 1, "agent-blocked.json")
        put(1, Make.agentId(5), 1, "agent-bucket.json")
        put(1, Make.agentId(6), 1) { it.put("key_name", "other-key") }
        put(2, Make.agentId(7), 1)
        val d = dir()
        val targets = d.mergeTargets(d.get(1, a1)!!)
        assertEquals(listOf(a3, a2), targets.map { it.agentId })
    }

    // ------------------------------------------------------------ sync.agents

    @Test
    fun theSyncFrameListsTheAgentsThatThePhoneHoldsWithTheirRev() {
        put(1, a1, 7)
        put(1, a2, 2)
        put(2, a3, 9)
        val refs = engine.syncFrame(1).agents!!
        assertEquals(setOf(a1 to 7L, a2 to 2L), refs.map { it.agentId to it.rev }.toSet())
        assertEquals(listOf(a3 to 9L), engine.syncFrame(2).agents!!.map { it.agentId to it.rev })
    }

    @Test
    fun theSyncFrameHasNoMemberAgentsWhenThePhoneHoldsNone() {
        assertNull(engine.syncFrame(1).agents)
        assertFalse(engine.syncFrame(1).toJson().has("agents"))
    }

    @Test
    fun theSyncListHasAtMost200AgentsTheLiveOnesAndTheRecentOnesFirst() {
        val agents = (0 until 230).map { n ->
            Agent(
                1, Make.hex(0x700 + n), 1, "ops-key", AgentKind.AGENT, if (n < 20) AgentState.BLOCKED else AgentState.KNOWN, "a$n", false, null,
                0, n.toLong(), 0, false, null, null, 0, false,
            )
        }
        val refs = AgentSync.refs(agents)!!
        assertEquals(MAX_SYNC_AGENTS, refs.size)
        assertEquals(refs.size, refs.map { it.agentId }.toSet().size)
        // The 20 blocked agents are the last ones: 210 live agents come first, so 10 of them are cut and the blocked ones stay at the end.
        assertTrue(refs.take(200).none { r -> agents.first { it.agentId == r.agentId }.state == AgentState.BLOCKED })
        // The most recently seen live agent is first.
        assertEquals(Make.hex(0x700 + 229), refs.first().agentId)
        // The frame validates.
        engine.let { SyncFrameCheck.ok(refs) }
    }

    @Test
    fun theNewAgentsListHoldsOnlyAgentsInStateNewAndNeverABucket() {
        put(1, a1, 1, "agent-new.json")
        put(1, a2, 1)
        put(1, a3, 1, "agent-bucket.json")
        assertEquals(listOf(a1), dir().newAgents().map { it.agentId })
    }
}

private object SyncFrameCheck {
    fun ok(refs: List<app.portalinbox.proto.AgentRef>) = app.portalinbox.proto.SyncFrame(emptyList(), refs).validate()
}
