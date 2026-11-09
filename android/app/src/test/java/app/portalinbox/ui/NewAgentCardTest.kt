package app.portalinbox.ui

import app.portalinbox.data.AgentActions
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.proto.AgentOp
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId

/** The card "New agent": the title, the hints, the warnings, the choices and the lock line. */
class NewAgentCardTest {
    private val zone = ZoneId.of("UTC")
    private val created = 1_788_000_000_000L
    private var now = created + 60_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val billing = Make.agentId(1)
    private val fresh = Make.agentId(3)
    private var n = 0

    private fun hold(server: Long, id: String, file: String = "agent-known.json", edit: (JSONObject) -> Unit = {}) {
        engine.onAgent(server, Make.agent(id, Make.hex(0x900 + n++), 1, file, edit))
    }

    private fun ask(server: Long, agent: String) {
        engine.onRequest(server, Make.approval(Make.hex(0x100 + n++), Make.hex(0x300 + n++), createdAtMs = now - 30_000) { Make.asAgent(it, agent, "x", "new") })
    }

    private fun cards(pending: Set<AgentOp> = emptySet()) =
        NewAgentCard.build(AgentDirectory(repo.allAgents()), repo.openItems(1), { now }, zone, { _, _ -> pending })

    private fun items(): List<Item> = repo.openItems(1)

    @Test
    fun aNewAgentHasACardWithTheKeyInTheTitleAndTheFirstCall() {
        hold(1, fresh, "agent-new.json") { it.put("created_at_ms", created) }
        ask(1, fresh)
        val c = cards().single()
        assertEquals("A new agent is using the key claude-laptop", c.title)
        assertEquals("First call today 10:40 \u00B7 1 request waits", c.subtitle)
        assertEquals("claude-laptop #4", c.portalLabel)
        assertEquals("Allow as \"claude-laptop #4\"", c.allowAsLabel)
        assertEquals(AgentTexts.NO_PROOF, c.noProof)
    }

    @Test
    fun theCountOfWaitingRequestsIsMadeByThePhone() {
        hold(1, fresh, "agent-new.json") { it.put("open", 7) }
        assertTrue(cards().single().subtitle.endsWith("no request waits"))
        ask(1, fresh)
        ask(1, fresh)
        assertTrue(cards().single().subtitle.endsWith("2 requests wait"))
    }

    @Test
    fun anAgentThatIsNotNewAndABucketHaveNoCard() {
        hold(1, billing)
        hold(1, Make.agentId(4), "agent-bucket.json")
        hold(1, Make.agentId(5), "agent-blocked.json")
        assertTrue(cards().isEmpty())
    }

    @Test
    fun theCardsFollowTheFilterOfThePortals() {
        hold(1, fresh, "agent-new.json")
        hold(2, fresh, "agent-new.json")
        val dir = AgentDirectory(repo.allAgents())
        val only2 = NewAgentCard.build(dir, emptyList(), { now }, zone, shows = { it == 2L })
        assertEquals(listOf(2L), only2.map { it.serverId })
    }

    @Test
    fun theObservedBoxHoldsTheHintsAndTheValuesAreTheTextOfAClient() {
        hold(1, fresh, "agent-new.json")
        val c = cards().single()
        assertEquals(
            listOf("Says it is" to "billing-bot (changed 1 time)", "Client" to "claude-code/2.1.0", "Address" to "203.0.113.0/24"),
            c.observed.map { it.label to it.value },
        )
    }

    @Test
    fun theLockLineShowsOnlyWithLockAccept() {
        hold(1, fresh, "agent-new.json")
        assertEquals(AgentTexts.LOCK_LINE, cards().single().lockLine)
        engine.onAgent(1, Make.agent(fresh, Make.hex(0xD00), 8, "agent-new.json") { it.put("lock_accept", false) })
        assertNull(cards().single().lockLine)
    }

    // ------------------------------------------------------------ warnings

    @Test
    fun aTextThatEqualsTheNameOfAKnownAgentOfTheKeyGivesTheCopyWarning() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, fresh, "agent-new.json") { it.getJSONObject("observed").put("says", "billing BOT").put("says_changed", null as Any?) ; it.remove("similar") }
        val w = cards().single().warnings
        assertEquals(listOf("Same text as your agent Billing bot.", AgentTexts.COPY_NOTE), w)
    }

    @Test
    fun aLookAlikeLetterGivesTheLookAlikeWarning() {
        hold(1, billing) { it.put("label", "Billing bot") }
        // A Cyrillic o in "bot".
        hold(1, fresh, "agent-new.json") { it.getJSONObject("observed").put("says", "billing b\u043Et").remove("says_changed"); it.remove("similar") }
        val w = cards().single().warnings
        assertEquals("Almost the same text as your agent Billing bot, with look-alike letters.", w.first())
        assertEquals(AgentTexts.COPY_NOTE, w.last())
    }

    @Test
    fun aNameOfAnotherKeyOrAnotherPortalIsNotCompared() {
        hold(1, billing) { it.put("label", "billing-bot").put("key_name", "other-key") }
        hold(2, Make.agentId(8)) { it.put("label", "billing-bot") }
        hold(1, fresh, "agent-new.json") { it.remove("similar") }
        assertTrue(cards().single().warnings.isEmpty())
    }

    @Test
    fun theSimilarHintOfThePortalIsShownWithTheCopyNoteAndNeverMerges() {
        hold(1, billing) { it.put("label", "Billing bot").put("observed", JSONObject().put("nets_seen", 1)) }
        hold(1, fresh, "agent-new.json") {
            it.getJSONObject("observed").put("says", "something else").remove("says_changed")
            it.getJSONObject("similar").put("agent_id", billing)
        }
        val c = cards().single()
        assertEquals(listOf("The portal thinks it looks like your agent Billing bot.", AgentTexts.COPY_NOTE), c.warnings)
        assertEquals("It is Billing bot: merge", c.mergeLabel)
        assertTrue(c.choices.single().looksLike)
    }

    @Test
    fun withNoSimilarHintTheMergeButtonOpensAPickerOfTheKnownAgents() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, Make.agentId(2)) { it.put("label", "Night ETL") }
        hold(1, fresh, "agent-new.json") { it.remove("similar"); it.getJSONObject("observed").put("says", "unrelated").remove("says_changed") }
        val c = cards().single()
        assertTrue(c.canMerge)
        assertEquals("Merge into an agent of this key", c.mergeLabel)
        assertEquals(listOf("Billing bot", "Night ETL"), c.choices.map { it.label })
        assertTrue(c.choices.none { it.looksLike })
        assertTrue(c.warnings.isEmpty())
    }

    @Test
    fun withNoKnownAgentOfTheKeyThereIsNoMergeButton() {
        hold(1, fresh, "agent-new.json") { it.remove("similar") }
        assertFalse(cards().single().canMerge)
    }

    @Test
    fun theAgentThatLooksLikeComesFirstInThePicker() {
        hold(1, Make.agentId(2)) { it.put("label", "Aardvark") }
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, fresh, "agent-new.json") { it.getJSONObject("similar").put("agent_id", billing) }
        assertEquals(listOf("Billing bot", "Aardvark"), cards().single().choices.map { it.label })
    }

    @Test
    fun theTextOfTheClientIsNeverCopiedIntoAWarningOrATitle() {
        hold(1, billing) { it.put("label", "Billing bot") }
        hold(1, fresh, "agent-new.json") { it.getJSONObject("observed").put("says", "BILLING-BOT-INJECTED \u00A0").remove("says_changed") }
        val c = cards().single()
        val all = (c.warnings + c.title + c.subtitle + c.portalLabel + c.allowAsLabel + c.mergeLabel + c.noProof).joinToString(" ")
        assertFalse(all.contains("INJECTED"))
    }

    // ------------------------------------------------------------ the form

    @Test
    fun theNameOfTheFormIsCheckedBeforeItGoesOut() {
        assertEquals(AgentActions.NameCheck.Ok("Refund cleaner"), NewAgentCard.nameAndAllow("  Refund cleaner "))
        assertEquals(AgentActions.NameCheck.Empty, NewAgentCard.nameAndAllow(""))
        assertEquals(AgentActions.NameCheck.BadCharacter, NewAgentCard.nameAndAllow("a\u200Fb"))
    }

    @Test
    fun theButtonsOfAnActionThatWaitsAreMarkedByThePending() {
        hold(1, fresh, "agent-new.json")
        assertEquals(setOf(AgentOp.ALLOW), cards(setOf(AgentOp.ALLOW)).single().pending)
    }

    @Test
    fun theFirstCallOfAnotherDayShowsTheDate() {
        assertEquals("today 10:40", NewAgentCard.firstCall(created, created + 60_000, zone))
        assertTrue(NewAgentCard.firstCall(created - 2 * 86_400_000, created, zone).matches(Regex("\\d\\d-\\d\\d \\d\\d:\\d\\d")))
    }
}
