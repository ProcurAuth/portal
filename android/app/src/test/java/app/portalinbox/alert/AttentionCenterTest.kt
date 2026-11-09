package app.portalinbox.alert

import app.portalinbox.proto.Urgency
import app.portalinbox.testsupport.AttentionWorld
import app.portalinbox.testsupport.FakeSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The center runs the one notification. A fake sink proves what the phone would post, play and cancel. */
class AttentionCenterTest {
    private val w = AttentionWorld()
    private val sink = FakeSink()
    private val center = AttentionCenter(sink)

    private fun update(
        items: List<app.portalinbox.data.Item> = emptyList(),
        alerts: List<app.portalinbox.data.Alert> = emptyList(),
        replies: List<Attention.Reply> = emptyList(),
        portals: List<Attention.Portal> = listOf(w.portal(w.production), w.portal(w.homelab)),
        force: Boolean = true,
        direct: Boolean = false,
    ) = center.update(w.input(portals, items, alerts, replies), force, direct)

    @Test
    fun itemsAlertsRepliesWarningsAndDigestsAllUseTheOneIdAndTheOneTag() {
        val a = w.approval()
        update(items = listOf(a))
        val q = w.question(2)
        update(items = listOf(a, q))
        update(items = listOf(a, q), alerts = listOf(w.alert(action = "ring")))
        update(items = listOf(a, q), alerts = listOf(w.alert(action = "notify")), replies = listOf(Attention.Reply(1, a.requestId, Urgency.NORMAL)))
        // A digest only refreshes.
        update(items = listOf(a, q), alerts = listOf(w.alert(action = "notify")), replies = listOf(Attention.Reply(1, a.requestId, Urgency.NORMAL)))
        // A portal goes down.
        update(
            items = listOf(a, q),
            portals = listOf(w.portal(w.production), w.portal(w.homelab, down = Attention.Down(Attention.DownKind.OFFLINE, 0))),
        )
        // An alarm.
        update(items = listOf(a, q), alerts = listOf(w.alert(action = "alarm")))
        assertTrue(sink.posts.size >= 6)
        assertEquals(setOf("attention" to 3000), sink.keys)
    }

    @Test
    fun aNewRequestPostsAndPlaysOnceAndNothingNewPostsWithoutSound() {
        val a = w.approval()
        update(items = listOf(a))
        assertEquals(listOf(Attention.Sound.RING), sink.played)
        assertEquals(Attention.CH_ATTENTION, sink.posts.last().channel)
        assertTrue(sink.posts.last().alert)
        // The same state again (a digest, a mark as read): the text refreshes, no sound.
        update(items = listOf(a))
        assertEquals(2, sink.posts.size)
        assertFalse(sink.posts.last().alert)
        assertEquals(1, sink.played.size)
    }

    @Test
    fun theAlertAgainRuleHoldsAcrossUpdates() {
        val high = w.approval(urgency = Urgency.HIGH)
        update(items = listOf(high))
        val played = sink.played.size
        // A less urgent arrival: the text updates in silence.
        val normal = w.question(2)
        update(items = listOf(high, normal))
        assertEquals(played, sink.played.size)
        assertFalse(sink.posts.last().alert)
        assertEquals("2 to answer", sink.posts.last().content.title)
        // A more urgent arrival: it rings again.
        update(items = listOf(high, normal), alerts = listOf(w.alert(action = "alarm")))
        assertTrue(sink.posts.last().alert)
    }

    @Test
    fun aTickPostsOnlyWhenTheShapeChanged() {
        val a = w.approval()
        update(items = listOf(a))
        assertEquals(1, sink.posts.size)
        update(items = listOf(a), force = false)
        assertEquals(1, sink.posts.size)
        update(items = listOf(a, w.question()), force = false)
        assertEquals(2, sink.posts.size)
    }

    @Test
    fun theNotificationIsCancelledWhenNothingWaits() {
        val a = w.approval()
        update(items = listOf(a))
        assertTrue(sink.shown)
        update(items = emptyList())
        assertFalse(sink.shown)
        assertEquals(1, sink.cancels)
        // Nothing waits and nothing changes: no second cancel from a tick.
        update(items = emptyList(), force = false)
        assertEquals(1, sink.cancels)
    }

    @Test
    fun anAlertThatIsMarkedReadLeavesTheNotificationAndTheLastOneCancelsIt() {
        val alert = w.alert(action = "ring")
        update(alerts = listOf(alert))
        assertEquals("1 alert", sink.posts.last().content.title)
        update(alerts = listOf(alert.copy(readAtMs = 1L)))
        assertFalse(sink.shown)
    }

    @Test
    fun aNotificationThatTheOwnerRemovedDoesNotComeBackForARefresh() {
        val a = w.approval()
        update(items = listOf(a))
        sink.dismiss()
        update(items = listOf(a))
        assertEquals(1, sink.posts.size)
        assertFalse(sink.shown)
        // A new arrival brings it back, even in silence.
        update(items = listOf(a, w.question(urgency = Urgency.LOW)))
        assertEquals(2, sink.posts.size)
        assertEquals(Attention.CH_QUIET, sink.posts.last().channel)
        assertFalse(sink.posts.last().alert)
    }

    @Test
    fun aRingingAlarmIsPostedEvenWhenTheOwnerRemovedTheNotification() {
        val alarm = w.alert(action = "alarm")
        update(alerts = listOf(alarm))
        sink.dismiss()
        update(alerts = listOf(alarm))
        assertEquals(2, sink.posts.size)
        assertTrue(sink.posts.last().alarm)
        // The alarm has its own player: the center plays no sound for it.
        assertTrue(sink.played.isEmpty())
    }

    @Test
    fun aNotificationOnScreenKeepsTheAlertingChannelWhenAQuietThingArrives() {
        val a = w.approval()
        update(items = listOf(a))
        update(items = listOf(a, w.question(urgency = Urgency.LOW)))
        assertEquals(Attention.CH_ATTENTION, sink.posts.last().channel)
        assertFalse(sink.posts.last().alert)
    }

    @Test
    fun theAlarmChannelTurnsQuietWhileThePageIsOnScreen() {
        val alarm = w.alert(action = "alarm")
        update(alerts = listOf(alarm))
        assertTrue(sink.posts.last().fullScreen)
        update(alerts = listOf(alarm), direct = true)
        assertEquals(Attention.CH_QUIET, sink.posts.last().channel)
        assertFalse(sink.posts.last().fullScreen)
    }

    @Test
    fun theSingleApprovalDeclineActionComesAndGoesWithTheState() {
        val a = w.approval()
        update(items = listOf(a))
        assertEquals(1L to a.requestId, sink.posts.last().decline)
        update(items = listOf(a, w.question()))
        assertNull(sink.posts.last().decline)
    }

    @Test
    fun theAgentIsBackIsANewArrivalThatAlertsAgain() {
        val a = w.approval()
        update(items = listOf(a))
        // The item was abandoned: it leaves. The agent revives it: it is a new thing again.
        update(items = emptyList())
        update(items = listOf(a))
        assertEquals(2, sink.played.size)
    }

    @Test
    fun noNetworkMakesNoWarningAndNoNotificationForPortalsAlone() {
        val down = Attention.Down(Attention.DownKind.OFFLINE, 0)
        val none = center.update(
            Attention.Input(listOf(w.portal(w.production, down = down)), emptyList(), emptyList(), emptyList(), w.now, network = false),
        )
        assertNull(none)
        assertTrue(sink.posts.isEmpty())
    }
}
