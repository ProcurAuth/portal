package app.portalinbox.ui

import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.AgentKey
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.portal.ItemSort
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The section "To answer" by agent: the groups, their order, the folding, the header and the rows. */
class AgentGroupsTest {
    private val t0 = 1_788_000_000_000L
    private var now = t0 + 3_600_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val billing = Make.agentId(1)
    private val etl = Make.agentId(2)
    private val fresh = Make.agentId(3)
    private val bucket = Make.agentId(4)
    private var n = 0

    private fun hold(server: Long, id: String, file: String = "agent-known.json", edit: (JSONObject) -> Unit = {}) {
        engine.onAgent(server, Make.agent(id, Make.hex(0x900 + n++), 1, file, edit))
    }

    private fun ask(server: Long, agent: String, label: String, ageMin: Long, state: String = "known", kind: String = "agent"): Item {
        val rid = Make.hex(0x100 + n++)
        engine.onRequest(server, Make.approval(rid, Make.hex(0x300 + n++), createdAtMs = now - ageMin * 60_000) { Make.asAgent(it, agent, label, state, kind) })
        return repo.item(server, rid)!!
    }

    private fun groups(collapse: AgentCollapse = AgentCollapse(), offsets: Map<Long, Long> = emptyMap()): List<AgentGroups.Group> {
        val sorted = ItemSort.toAnswer(repo.allOpen(), offsets)
        return ItemListOrder.toAnswer(sorted, AgentDirectory(repo.allAgents()), collapse)
    }

    private fun MemoryItemRepository.allOpen(): List<Item> = listOf(1L, 2L).flatMap { openItems(it) }

    // ------------------------------------------------------------ order

    @Test
    fun theGroupsComeByTheirOldestOpenItem() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, etl) { it.put("label", "Night ETL") }
        ask(1, etl, "Night ETL", 120)
        ask(1, billing, "Billing bot", 30)
        ask(1, billing, "Billing bot", 200)
        ask(1, etl, "Night ETL", 10)
        val g = groups()
        assertEquals(listOf(billing, etl), g.map { it.agentId })
        assertEquals(listOf(2, 2), g.map { it.count })
    }

    @Test
    fun insideAGroupTheOldestIsFirst() {
        hold(1, billing)
        val young = ask(1, billing, "Billing bot", 5)
        val old = ask(1, billing, "Billing bot", 50)
        val mid = ask(1, billing, "Billing bot", 20)
        assertEquals(listOf(old, mid, young).map { it.requestId }, groups().single().items.map { it.requestId })
    }

    @Test
    fun newAgentsComeFirstThenTheOldestGroupsAndTheBucketIsLast() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, fresh, "agent-new.json") { it.put("label", "ops-key #4") }
        hold(1, bucket, "agent-bucket.json") { it.put("label", "Unattributed · ops-key") }
        ask(1, bucket, "Unattributed", 500, kind = "bucket")
        ask(1, billing, "Billing bot", 100)
        ask(1, fresh, "ops-key #4", 1, state = "new")
        val g = groups()
        assertEquals(listOf(fresh, billing, bucket), g.map { it.agentId })
        assertTrue(g[0].isNew)
        assertTrue(g[2].isBucket)
    }

    @Test
    fun twoNewAgentsKeepTheOrderOfTheirOldestItem() {
        hold(1, fresh, "agent-new.json")
        hold(1, etl, "agent-new.json")
        ask(1, etl, "x", 10, state = "new")
        ask(1, fresh, "y", 90, state = "new")
        assertEquals(listOf(fresh, etl), groups().map { it.agentId })
    }

    @Test
    fun groupsOfTwoPortalsNeverMergeEvenWithTheSameAgentId() {
        hold(1, billing) { it.put("label", "On portal one") }
        hold(2, billing) { it.put("label", "On portal two") }
        ask(1, billing, "x", 10)
        ask(2, billing, "x", 20)
        val g = groups()
        assertEquals(2, g.size)
        assertEquals(setOf(AgentKey.of(1, billing), AgentKey.of(2, billing)), g.map { it.key }.toSet())
        assertEquals(setOf("On portal one", "On portal two"), g.map { it.name }.toSet())
    }

    @Test
    fun theOrderAcrossPortalsFollowsTheClaimedAgeLimitOfItemSort() {
        hold(1, billing)
        hold(2, etl)
        // The portal 2 claims an item of 100 days: the sort key is limited to 7 days before the arrival.
        engine.onRequest(2, Make.approval(Make.hex(0x77), Make.hex(0x78), createdAtMs = 1L) { Make.asAgent(it, etl, "x") })
        ask(1, billing, "x", 60 * 24 * 8)
        val g = groups()
        assertEquals(2, g.size)
    }

    // ------------------------------------------------------------ merges and renames

    @Test
    fun anItemOfAMergedAgentIsInTheGroupOfTheTarget() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, etl, "agent-merged.json") { it.put("merged_into", billing) }
        ask(1, etl, "claude-laptop #3", 30)
        ask(1, billing, "Billing bot", 10)
        val g = groups().single()
        assertEquals(billing, g.agentId)
        assertEquals("Billing bot", g.name)
        assertEquals(2, g.count)
    }

    @Test
    fun aRenameChangesTheNameOfTheGroupAtOnce() {
        hold(1, billing) { it.put("label", "ops-key #3") }
        ask(1, billing, "ops-key #3", 30)
        assertEquals("ops-key #3", groups().single().name)
        engine.onAgent(1, Make.agent(billing, Make.hex(0xA00), 2) { it.put("label", "Billing bot").put("named", true) })
        assertEquals("Billing bot", groups().single().name)
    }

    @Test
    fun anAgentThatThePhoneDoesNotHoldUsesTheFrameOfTheItem() {
        ask(1, billing, "Name of the frame", 30, state = "new")
        val g = groups().single()
        assertEquals("Name of the frame", g.name)
        assertNull(g.agent)
        assertNull(g.lastCallMs)
        assertTrue(g.isNew)
        assertNull(g.lockLine)
    }

    @Test
    fun theCountIsMadeByThePhoneNotByTheSnapshotOfTheFrame() {
        hold(1, billing) { it.put("open", 9) }
        ask(1, billing, "x", 30)
        assertEquals(1, groups().single().count)
    }

    @Test
    fun aClosedItemMakesNoGroup() {
        hold(1, billing)
        val i = ask(1, billing, "x", 30)
        engine.onSettled(1, Make.settled(i.requestId, Make.hex(0xB00)))
        assertTrue(groups().isEmpty())
    }

    // ------------------------------------------------------------ folding

    @Test
    fun aFoldedGroupShowsNoRowButKeepsItsCount() {
        hold(1, billing)
        hold(1, etl)
        ask(1, billing, "x", 30)
        ask(1, billing, "x", 20)
        ask(1, etl, "x", 10)
        val key = AgentKey.of(1, billing)
        val g = groups(AgentCollapse().toggle(key))
        val folded = g.first { it.key == key }
        assertTrue(folded.folded)
        assertTrue(folded.shown.isEmpty())
        assertEquals(2, folded.count)
        assertFalse(g.first { it.agentId == etl }.folded)
        assertEquals(1, g.first { it.agentId == etl }.shown.size)
    }

    @Test
    fun theFoldingOfOnePortalDoesNotFoldTheSameAgentIdOfAnother() {
        hold(1, billing)
        hold(2, billing)
        ask(1, billing, "x", 30)
        ask(2, billing, "x", 20)
        val g = groups(AgentCollapse().toggle(AgentKey.of(1, billing)))
        assertTrue(g.first { it.serverId == 1L }.folded)
        assertFalse(g.first { it.serverId == 2L }.folded)
    }

    // ------------------------------------------------------------ header and hint

    @Test
    fun theHeaderLineNamesTheKeyAndTheLastCall() {
        hold(1, billing) { it.put("last_seen_ms", now - 60_000) }
        ask(1, billing, "x", 30)
        val g = groups().single()
        assertEquals("ops-key · last call 1 min ago", AgentGroups.keyLine(g, now))
    }

    @Test
    fun theHeaderLineOfANewAgentAndOfTheBucket() {
        hold(1, fresh, "agent-new.json")
        hold(1, bucket, "agent-bucket.json")
        ask(1, fresh, "ops-key #4", 30, state = "new")
        ask(1, bucket, "Unattributed", 30, kind = "bucket")
        val g = groups()
        assertEquals("ops-key · new, not named", AgentGroups.keyLine(g.first { it.isNew }, now))
        assertEquals("ops-key · agent not known", AgentGroups.keyLine(g.first { it.isBucket }, now))
    }

    @Test
    fun theHintLineHoldsTheObservedPartsAndTheBucketSaysNotKnown() {
        hold(1, fresh, "agent-new.json")
        hold(1, bucket, "agent-bucket.json")
        ask(1, fresh, "x", 30, state = "new")
        ask(1, bucket, "x", 30, kind = "bucket")
        val g = groups()
        val h = AgentGroups.hint(g.first { it.isNew })!!
        assertEquals("OBSERVED", h.label)
        assertEquals(listOf("says \"billing-bot\"", "claude-code/2.1.0", "203.0.113.0/24"), h.parts)
        assertTrue(h.untrusted)
        val b = AgentGroups.hint(g.first { it.isBucket })!!
        assertEquals("NOT KNOWN", b.label)
        assertFalse(b.untrusted)
        assertNull(g.first { it.isBucket }.observed)
    }

    @Test
    fun aGroupWithNoObservedTextHasNoHintLine() {
        hold(1, billing) { it.remove("observed") }
        ask(1, billing, "x", 30)
        assertNull(AgentGroups.hint(groups().single()))
    }

    @Test
    fun theLockLineShowsOnlyWhenTheServerLocksTheNewAgent() {
        hold(1, fresh, "agent-new.json")
        ask(1, fresh, "x", 30, state = "new")
        assertEquals("Approve is locked until you allow this agent.", groups().single().lockLine)
        // lock_new false, the default: the frame says lock_accept false and there is no lock line.
        engine.onAgent(1, Make.agent(fresh, Make.hex(0xC00), 9, "agent-new.json") { it.put("lock_accept", false) })
        assertNull(groups().single().lockLine)
    }

    @Test
    fun theHeaderIsSpokenWithTheWordsOfTheStateAndTheFolding() {
        hold(1, billing) { it.put("label", "Billing bot") }
        ask(1, billing, "x", 30)
        val g = groups().single()
        val said = AgentGroups.spoken(g, now)
        assertTrue(said, said.startsWith("Agent Billing bot, key claude-laptop, known agent"))
        assertTrue(said.contains("1 request, open. Double tap to fold."))
        assertTrue(AgentGroups.spoken(g.copy(folded = true), now).endsWith("folded. Double tap to open."))
    }

    @Test
    fun aHiddenCharacterInANameShowsAsAMark() {
        hold(1, billing) { it.put("label", "Bill‮ing") }
        ask(1, billing, "x", 30)
        assertEquals("Bill<U+202E>ing", groups().single().name)
    }

    // ------------------------------------------------------------ the rows under the header

    @Test
    fun aRowUnderTheHeaderDoesNotRepeatTheNameOfTheAgent() {
        hold(1, billing) { it.put("label", "Billing bot") }
        val i = ask(1, billing, "Billing bot", 30)
        val grouped = ItemListOrder.row(i, now, label = "Billing bot", grouped = true)
        assertEquals("datastore", grouped.subtitle)
        assertEquals("Billing bot · datastore", ItemListOrder.row(i, now, label = "Billing bot", grouped = false).subtitle)
        // The old call keeps working: the name of the frame.
        assertEquals("Billing bot · datastore", ItemListOrder.row(i, now).subtitle)
    }

    @Test
    fun aRowTakesTheNameOfTheTableWhenItIsNotGrouped() {
        hold(1, billing) { it.put("label", "Renamed") }
        val i = ask(1, billing, "Old name", 30)
        val dir = AgentDirectory(repo.allAgents())
        assertEquals("Renamed · datastore", ItemListOrder.row(i, now, label = dir.labelOf(i)).subtitle)
        assertTrue(TalkBackLabels.itemRow(i, now, label = dir.labelOf(i)).contains("from Renamed"))
        assertTrue(TalkBackLabels.itemRow(i, now).contains("from Old name"))
    }

    @Test
    fun theKindsOfTheStatesAreTheOnesOfTheFrame() {
        hold(1, fresh, "agent-new.json")
        ask(1, fresh, "x", 30, state = "new")
        val g = groups().single()
        assertEquals(AgentState.NEW, g.state)
        assertEquals(AgentKind.AGENT, g.kind)
    }
}
