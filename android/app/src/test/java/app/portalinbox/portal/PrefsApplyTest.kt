package app.portalinbox.portal

import app.portalinbox.data.PendingPrefs
import app.portalinbox.portal.PrefsApply.Action
import app.portalinbox.portal.PrefsApply.Reason
import app.portalinbox.portal.PrefsApply.Target
import app.portalinbox.proto.DigestMode
import app.portalinbox.proto.DigestPrefs
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.SetPrefsFrame
import app.portalinbox.service.LinkState
import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** "Apply to all portals": who gets which values, what waits, what is skipped, and that nothing runs by itself. */
class PrefsApplyTest {
    private val values = Prefs(QuietPrefs("22:00", "07:00", "Europe/Paris", true), DigestPrefs(DigestMode.DAILY, "09:00", "Europe/Paris", null))
    private val other = Prefs(QuietPrefs("20:00", "06:00", "Europe/Paris", false), null)

    private class Memory : PendingPrefs {
        val map = HashMap<Long, Prefs>()
        override fun get(serverId: Long) = map[serverId]
        override fun set(serverId: Long, prefs: Prefs) { map[serverId] = prefs }
        override fun clear(serverId: Long) { map.remove(serverId) }
    }

    private fun target(id: Long, name: String, state: LinkState, items: Boolean = true, account: String = "alice", effective: Prefs? = other) =
        Target(Portals.identity(id, name, id.toInt()), state, items, account, effective)

    private val staging = target(2, "Staging", LinkState.CONNECTED)
    private val homelab = target(3, "Homelab", LinkState.DISCONNECTED)
    private val clientA = target(4, "Client A", LinkState.KEY_CHANGED)

    private fun plan(vararg t: Target, source: String = "alice") = PrefsApply.plan(source, t.toList(), values)

    // ------------------------------------------------------------ what each portal gets

    @Test
    fun aConnectedPortalGetsTheFrameNow() {
        val s = plan(staging).single()
        assertEquals(Action.SEND, s.action)
        assertEquals(Reason.NOW, s.reason)
        assertEquals("Sent now", s.text)
        assertTrue(s.selectable && s.ticked)
    }

    @Test
    fun anOfflinePortalIsQueuedForItsNextWelcome() {
        for (state in listOf(LinkState.DISCONNECTED, LinkState.CONNECTING)) {
            val s = plan(target(3, "Homelab", state)).single()
            assertEquals(Action.QUEUE, s.action)
            assertEquals("Offline. Sent when it is back.", s.text)
            assertTrue(s.ticked)
        }
    }

    @Test
    fun aPortalWithAChangedKeyOrARevokedDeviceIsSkippedAndCannotBeTicked() {
        for (state in listOf(LinkState.KEY_CHANGED, LinkState.REENROLL)) {
            val s = plan(target(4, "Client A", state)).single()
            assertEquals(Action.SKIP, s.action)
            assertEquals("Not connected. Not sent.", s.text)
            assertFalse(s.selectable || s.ticked)
        }
    }

    @Test
    fun aPortalWithoutRequestsIsSkipped() {
        val s = plan(target(5, "Plain", LinkState.CONNECTED, items = false)).single()
        assertEquals(Action.SKIP, s.action)
        assertEquals(Reason.ITEMS_OFF, s.reason)
        assertEquals("No requests on this portal. Not sent.", s.text)
    }

    @Test
    fun aPortalThatAlreadyHasTheValuesIsSkipped() {
        val s = plan(target(2, "Staging", LinkState.CONNECTED, effective = values)).single()
        assertEquals(Action.SKIP, s.action)
        assertEquals(Reason.SAME, s.reason)
    }

    @Test
    fun theSheetKeepsTheOrderOfThePortals() {
        val steps = plan(staging, homelab, clientA)
        assertEquals(listOf("Staging", "Homelab", "Client A"), steps.map { it.target.identity.nickname })
        assertEquals(listOf(Action.SEND, Action.QUEUE, Action.SKIP), steps.map { it.action })
    }

    @Test
    fun aDifferentAccountNameIsShown() {
        val s = plan(target(2, "Staging", LinkState.CONNECTED, account = "a.smith"), source = "alice").single()
        assertEquals("Account @a.smith. Its values are replaced.", s.accountText)
        assertNull(plan(staging, source = "alice").single().accountText)
    }

    // ------------------------------------------------------------ running: only what the user chose

    @Test
    fun planningWritesNothing() {
        val pending = Memory()
        plan(staging, homelab, clientA)
        assertTrue(pending.map.isEmpty())
    }

    @Test
    fun theChosenPortalsGetTheirPendingEditAndTheFrame() {
        val pending = Memory()
        val sent = ArrayList<Pair<Long, SetPrefsFrame>>()
        val steps = plan(staging, homelab, clientA)
        val out = PrefsApply.run(steps, setOf(2L, 3L), values, pending, { id, f -> sent.add(id to f) })
        assertEquals(listOf(2L), out.sent)
        assertEquals(listOf(3L), out.queued)
        assertEquals(values, pending.get(2))
        assertEquals(values, pending.get(3))
        assertEquals(listOf(2L to SetPrefsFrame(values), 3L to SetPrefsFrame(values)), sent)
    }

    @Test
    fun aPortalThatTheUserDidNotChooseIsNeverTouched() {
        val pending = Memory()
        val sent = ArrayList<Long>()
        PrefsApply.run(plan(staging, homelab), setOf(3L), values, pending, { id, _ -> sent.add(id) })
        assertNull(pending.get(2))
        assertEquals(listOf(3L), sent)
    }

    @Test
    fun aSkippedPortalIsLeftAloneEvenWhenItIsChosen() {
        val pending = Memory()
        val sent = ArrayList<Long>()
        val out = PrefsApply.run(plan(clientA), setOf(4L), values, pending, { id, _ -> sent.add(id) })
        assertTrue(out.sent.isEmpty() && out.queued.isEmpty())
        assertTrue(sent.isEmpty())
        assertNull(pending.get(4))
    }

    @Test
    fun aNewerManualChangeReplacesTheQueuedValue() {
        val pending = Memory()
        PrefsApply.run(plan(homelab), setOf(3L), values, pending, { _, _ -> })
        // The user edits the portal later: the pending value is the newer one.
        pending.set(3, other)
        assertEquals(other, pending.get(3))
    }

    @Test
    fun noChosenPortalMeansNoResultText() {
        val out = PrefsApply.run(plan(staging), emptySet(), values, Memory(), { _, _ -> })
        assertNull(PrefsApply.resultText(out))
    }

    // ------------------------------------------------------------ texts

    @Test
    fun theButtonAndTheResultNameTheNumbers() {
        assertEquals("Apply to 1 portal", PrefsApply.buttonText(1))
        assertEquals("Apply to 2 portals", PrefsApply.buttonText(2))
        assertEquals("Sent to 1 portal.", PrefsApply.resultText(PrefsApply.Outcome(listOf(2), emptyList())))
        assertEquals("Sent to 2 portals. 1 portal gets it when back online.", PrefsApply.resultText(PrefsApply.Outcome(listOf(2, 3), listOf(4))))
        assertEquals("2 portals get it when back online.", PrefsApply.resultText(PrefsApply.Outcome(emptyList(), listOf(2, 3))))
    }

    @Test
    fun theSheetSaysTheValuesAndTheZoneThatIsSent() {
        assertEquals(
            "Quiet hours 22:00 to 07:00 (Europe/Paris) and a daily summary at 09:00. Urgent items still ring. Each portal keeps its own account.",
            PrefsApply.describe(values),
        )
        val tokyo = Prefs(QuietPrefs("23:00", "06:00", "Asia/Tokyo", false), DigestPrefs(DigestMode.DAILY, "08:00", "Europe/Paris", null))
        assertEquals(
            "Quiet hours 23:00 to 06:00 (Asia/Tokyo) and a daily summary at 08:00 (Europe/Paris). Each portal keeps its own account.",
            PrefsApply.describe(tokyo),
        )
        assertEquals("No quiet hours and the default summary of each portal. Each portal keeps its own account.", PrefsApply.describe(Prefs.EMPTY))
        assertEquals(
            "No quiet hours and a summary every 4 h. Each portal keeps its own account.",
            PrefsApply.describe(Prefs(null, DigestPrefs(DigestMode.EVERY, null, null, 4))),
        )
        assertEquals(
            "No quiet hours and no summary. Each portal keeps its own account.",
            PrefsApply.describe(Prefs(null, DigestPrefs(DigestMode.OFF, null, null, null))),
        )
    }

    @Test
    fun noTextOfTheSheetSaysServerOrNamesAPortalInTheValues() {
        val all = plan(staging, homelab, clientA).flatMap { listOfNotNull(it.text, it.accountText) } + PrefsApply.describe(values)
        for (t in all) assertFalse(t, t.contains("server", ignoreCase = true))
    }
}
