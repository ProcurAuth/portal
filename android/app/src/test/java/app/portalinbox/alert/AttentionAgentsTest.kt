package app.portalinbox.alert

import app.portalinbox.alert.Attention.Sound
import app.portalinbox.proto.Urgency
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.testsupport.AttentionWorld
import app.portalinbox.testsupport.FakeSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A new agent in the one notification: a count and a line, never a second notification, never a text of the agent. */
class AttentionAgentsTest {
    private val w = AttentionWorld()
    private val sink = FakeSink()
    private val center = AttentionCenter(sink)

    private fun compute(input: Attention.Input) = Attention.compute(input)

    private fun update(
        items: List<app.portalinbox.data.Item> = emptyList(),
        agents: List<Attention.NewAgent> = emptyList(),
        portals: List<Attention.Portal> = listOf(w.portal(w.production), w.portal(w.homelab)),
        force: Boolean = true,
    ) = center.update(w.input(portals, items, emptyList(), emptyList(), agents = agents), force, false)

    // ------------------------------------------------------------ the aggregate

    @Test
    fun aNewAgentIsACountAndALineOfThePortal() {
        val a = compute(w.input(agents = listOf(w.newAgent())))
        assertEquals(1, a.newAgents)
        assertFalse(a.isEmpty)
        val t = AttentionText.of(a)
        assertEquals("1 new agent", t.title)
        assertEquals(listOf("Production: new agent on ops-key"), t.lines)
    }

    @Test
    fun theNewAgentIsInTheCountsNextToTheOthers() {
        val a = compute(w.input(items = listOf(w.approval(), w.question()), agents = listOf(w.newAgent(), w.newAgent(keyName = "build-key"))))
        val t = AttentionText.of(a)
        assertEquals("2 to answer, 2 new agents", t.title)
        assertEquals(listOf("Production: 2 to answer · 2 new agents on ops-key, build-key"), t.lines)
    }

    @Test
    fun severalPortalsHaveOneLineEachWithTheirOwnNewAgents() {
        val a = compute(
            w.input(
                portals = listOf(w.portal(w.production), w.portal(w.homelab)),
                items = listOf(w.approval(1), w.approval(2)),
                agents = listOf(w.newAgent(2, "lab-key")),
            ),
        )
        val t = AttentionText.of(a)
        assertEquals("Production: 1 to answer", t.lines[0])
        assertEquals("Homelab: 1 to answer · new agent on lab-key", t.lines[1])
    }

    @Test
    fun anAgentOfARemovedPortalDoesNotCount() {
        val a = compute(w.input(agents = listOf(w.newAgent(server = 9))))
        assertEquals(0, a.newAgents)
        assertTrue(a.isEmpty)
    }

    @Test
    fun theLockScreenHasTheCountAndNoKeyName() {
        val a = compute(w.input(items = listOf(w.approval()), agents = listOf(w.newAgent(keyName = "secret-key"))))
        val t = AttentionText.of(a)
        assertEquals("1 to answer, 1 new agent", t.publicTitle)
        for (text in listOf(t.publicTitle, t.publicText)) assertFalse(text, text.contains("secret-key"))
        // The private version has it, from the portal.
        assertTrue(t.lines.any { it.contains("secret-key") })
    }

    @Test
    fun theKeyNameGoesThroughTheEscapeOfHiddenCharacters() {
        val a = compute(w.input(agents = listOf(w.newAgent(keyName = "ops‮key"))))
        val line = AttentionText.of(a).lines.single()
        assertFalse(line.contains('‮'))
        assertTrue(line, line.contains("ops<U+202E>key"))
    }

    @Test
    fun aNewAgentOfAPortalThatIsDownIsInTheSameLine() {
        val down = Attention.Down(Attention.DownKind.OFFLINE, 0L)
        val a = compute(w.input(portals = listOf(w.portal(w.production, down = down)), agents = listOf(w.newAgent())))
        assertEquals(1, a.lines.size)
        assertTrue(AttentionText.of(a).lines.single().contains("new agent on ops-key"))
    }

    @Test
    fun theDeclineActionIsOfferedForOneApprovalOnlyWithNoNewAgent() {
        val one = w.approval()
        assertNotNull(compute(w.input(items = listOf(one))).singleApproval)
        assertNull(compute(w.input(items = listOf(one), agents = listOf(w.newAgent()))).singleApproval)
    }

    // ------------------------------------------------------------ the sound

    @Test
    fun aNewAgentAlertsOnceWithTheSoundOfThePhone() {
        val a = compute(w.input(agents = listOf(w.newAgent())))
        val d = Attention.decide(emptyMap(), a)
        assertTrue(d.alert)
        assertEquals(Sound.PHONE, d.sound)
    }

    @Test
    fun aMutedPortalAndTheQuietWindowMakeNoSound() {
        val muted = compute(w.input(portals = listOf(w.portal(w.production, muted = true)), agents = listOf(w.newAgent())))
        assertFalse(Attention.decide(emptyMap(), muted).alert)
        assertTrue(Attention.decide(emptyMap(), muted).arrivals)
    }

    @Test
    fun anAgentThatThePhoneAlreadyShowedIsNotNewAfterARestart() {
        val a = compute(w.input(agents = listOf(w.newAgent(notified = true))))
        val d = Attention.decide(null, a)
        assertFalse(d.alert)
        assertFalse(d.arrivals)
    }

    // ------------------------------------------------------------ the center

    @Test
    fun theNewAgentUsesTheOneIdAndTheOneTagAndNeverASecondNotification() {
        val agent = w.newAgent()
        val approval = w.approval()
        update(items = listOf(approval))
        update(items = listOf(approval), agents = listOf(agent))
        update(items = listOf(approval), agents = listOf(agent, w.newAgent()))
        update(agents = listOf(agent))
        assertEquals(setOf("attention" to 3000), sink.keys)
        assertTrue(sink.posts.size >= 3)
        assertTrue(sink.posts.any { it.content.title.contains("new agent") })
    }

    @Test
    fun theSameAgentAndTheSameStateNeverAlertAgain() {
        val agent = w.newAgent()
        update(agents = listOf(agent))
        assertEquals(1, sink.played.size)
        assertEquals(listOf(Sound.PHONE), sink.played)
        // A tick, a refresh, an update of the same agent: no sound, no new post.
        update(agents = listOf(agent), force = false)
        update(agents = listOf(agent), force = true)
        assertEquals(1, sink.played.size)
    }

    @Test
    fun theNotificationLeavesWhenTheAgentIsDecidedAndNothingElseWaits() {
        val agent = w.newAgent()
        update(agents = listOf(agent))
        assertTrue(sink.shown)
        update(agents = emptyList())
        assertFalse(sink.shown)
        assertEquals(1, sink.cancels)
    }

    @Test
    fun aSecondNewAgentAlertsAgainBecauseItIsNewToo() {
        update(agents = listOf(w.newAgent()))
        update(agents = listOf(w.newAgent(id = "00000000000000000000000000000f01"), w.newAgent(id = "00000000000000000000000000000f02")))
        assertEquals(2, sink.played.size)
    }

    @Test
    fun theTextHoldsNoTextThatAnAgentSent() {
        // The notification reads the name of the key and nothing else of the agent: the type has no field for a label or a says.
        val fields = Attention.NewAgent::class.java.declaredFields.map { it.name }.toSet()
        assertEquals(setOf("serverId", "agentId", "keyName", "notified"), fields)
    }
}
