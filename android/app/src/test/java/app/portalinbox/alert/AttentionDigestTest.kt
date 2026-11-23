package app.portalinbox.alert

import app.portalinbox.alert.Attention.Sound
import app.portalinbox.proto.Urgency
import app.portalinbox.testsupport.AttentionWorld
import app.portalinbox.testsupport.FakeSink
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The digest inside the one notification (decision "one notification"): a refresh with numbers, never a second notification. */
class AttentionDigestTest {
    private val w = AttentionWorld()
    private val day = 86_400_000L
    private val hour = 3_600_000L
    private val two = listOf(w.portal(w.production), w.portal(w.homelab))

    private fun compute(input: Attention.Input) = Attention.compute(input)

    @Test
    fun aDigestIsASilentEntryOfTheSameNotification() {
        val a = compute(w.input(items = listOf(w.approval(), w.question()), digests = listOf(w.waiting(1, 2, 6 * day))))
        val e = a.entries.single { it.what == Attention.What.DIGEST }
        assertEquals(Sound.NONE, e.sound)
        assertEquals(Attention.Rank.LOW, e.rank)
        // The counts of the items do not change.
        assertEquals(2, a.toAnswer)
        assertEquals(2L, a.waiting)
    }

    @Test
    fun aDigestNeverMakesASound() {
        val a = compute(w.input(digests = listOf(w.waiting(1, 3, 2 * day))))
        val d = Attention.decide(emptyMap(), a)
        assertFalse(d.alert)
        assertEquals(Sound.NONE, d.sound)
        // A new digest is an arrival: the text refreshes.
        assertTrue(d.arrivals)
    }

    @Test
    fun theLineOfAPortalAddsTheWaitingPart() {
        val a = compute(w.input(portals = two, items = listOf(w.question(1), w.question(1), w.question(2)), digests = listOf(w.waiting(1, 2, 6 * day))))
        val t = AttentionText.of(a)
        assertEquals("3 to answer", t.title)
        assertEquals(listOf("Production: 2 to answer · 2 waiting, oldest 6 days", "Homelab: 1 to answer"), t.lines)
    }

    @Test
    fun theMergedDigestHasALineForEachPortalWithADigest() {
        val a = compute(
            w.input(
                portals = two, items = listOf(w.approval(1, ageMs = 6 * day), w.question(2, ageMs = 3 * hour)),
                digests = listOf(w.waiting(1, 1, 6 * day), w.waiting(2, 1, 3 * hour)),
            ),
        )
        val t = AttentionText.of(a)
        assertEquals(listOf("Production: 1 to answer · 1 waiting, oldest 6 days", "Homelab: 1 to answer · 1 waiting, oldest 3 hours"), t.lines)
        assertEquals(2L, a.waiting)
    }

    @Test
    fun aSinglePortalWithADigestShowsItsLine() {
        val a = compute(w.input(items = listOf(w.question(1, ageMs = 2 * day)), digests = listOf(w.waiting(1, 1, 2 * day))))
        assertEquals(listOf("Production: 1 to answer · 1 waiting, oldest 2 days"), AttentionText.of(a).lines)
    }

    @Test
    fun theDigestUsesTheClockOfItsPortal() {
        val skewed = Attention.Portal(w.homelab, false, false, null, w.now + 2 * hour)
        val a = compute(w.input(portals = listOf(w.portal(w.production), skewed), digests = listOf(w.waiting(2, 1, hour))))
        assertEquals(listOf("Homelab: 1 waiting, oldest 3 hours"), AttentionText.of(a).lines)
        assertEquals(3 * hour, a.oldestAgeMs)
    }

    @Test
    fun whenThePhoneHoldsNoItemYetTheDigestGivesTheTitle() {
        val a = compute(w.input(portals = two, digests = listOf(w.waiting(1, 3, 6 * day), w.waiting(2, 2, hour))))
        val t = AttentionText.of(a)
        assertEquals("5 waiting", t.title)
        assertEquals("5 waiting", t.publicTitle)
        assertEquals("Production, Homelab · The oldest waits 6 days", t.text)
        assertEquals("Production, Homelab · Unlock to see them.", t.publicText)
        assertEquals(listOf("Production: 3 waiting, oldest 6 days", "Homelab: 2 waiting, oldest 1 hour"), t.lines)
    }

    @Test
    fun aDigestAndAPortalThatIsDownShareTheNotification() {
        val down = Attention.Down(Attention.DownKind.OFFLINE, w.now - 3 * hour)
        val a = compute(w.input(portals = listOf(w.portal(w.production), w.portal(w.homelab, down = down)), digests = listOf(w.waiting(1, 2, day))))
        val t = AttentionText.of(a)
        assertEquals("2 waiting", t.title)
        assertTrue(t.text.endsWith("Homelab is not connected"))
        assertEquals(2, t.lines.size)
    }

    @Test
    fun theLockScreenHoldsNumbersAndTheAllowedNamesOnly() {
        val a = compute(
            w.input(
                portals = listOf(w.portal(w.production), w.portal(w.homelab, hide = true)),
                items = listOf(w.question(1), w.question(2)), digests = listOf(w.waiting(1, 1, 6 * day), w.waiting(2, 1, 6 * day)),
            ),
        )
        val t = AttentionText.of(a)
        assertEquals("2 to answer", t.publicTitle)
        assertEquals("Production · Unlock to see them.", t.publicText)
        assertFalse(t.publicText.contains("waiting"))
    }

    @Test
    fun aDigestOfARemovedPortalDoesNotCount() {
        val a = compute(w.input(portals = listOf(w.portal(w.production)), digests = listOf(w.waiting(9, 4, day))))
        assertTrue(a.isEmpty)
        assertEquals(0L, a.waiting)
    }

    @Test
    fun noDigestAndNoItemMeansNoNotification() {
        assertTrue(compute(w.input()).isEmpty)
    }

    @Test
    fun aDigestThatSaysMoreWaitsTakesTheDeclineAction() {
        val a = w.approval()
        assertEquals(1L to a.requestId, Attention.plan(compute(w.input(items = listOf(a), digests = listOf(w.waiting(1, 1, hour)))), Attention.decide(emptyMap(), compute(w.input(items = listOf(a)))), false, false).decline)
        val more = compute(w.input(items = listOf(a), digests = listOf(w.waiting(1, 3, hour))))
        assertNull(Attention.plan(more, Attention.decide(emptyMap(), more), false, false).decline)
        assertNull(more.singleApproval)
    }

    @Test
    fun theShapeKeyChangesWithANewDigest() {
        val a = w.input(items = listOf(w.question()), digests = listOf(w.waiting(1, 1, hour, id = "d1")))
        val b = w.input(items = listOf(w.question()), digests = listOf(w.waiting(1, 1, hour, id = "d2")))
        assertFalse(compute(a).key == compute(b).key)
    }

    // ------------------------------------------------------------ the center: one id, one tag, no sound

    private val sink = FakeSink()
    private val center = AttentionCenter(sink)

    @Test
    fun aDigestRefreshesTheNotificationWithoutASoundOrASecondId() {
        val q = w.question(urgency = Urgency.NORMAL)
        center.update(w.input(items = listOf(q)))
        val sounds = sink.played.size
        val posts = sink.posts.size
        center.update(w.input(items = listOf(q), digests = listOf(w.waiting(1, 1, 3 * hour))))
        assertEquals(posts + 1, sink.posts.size)
        assertFalse(sink.posts.last().alert)
        assertEquals(sounds, sink.played.size)
        assertEquals(listOf("Production: 1 to answer · 1 waiting, oldest 3 hours"), sink.posts.last().content.lines)
        assertEquals(setOf("attention" to 3000), sink.keys)
    }

    @Test
    fun aDigestBringsBackTheRemovedNotificationInSilence() {
        val q = w.question()
        center.update(w.input(items = listOf(q)))
        sink.dismiss()
        val sounds = sink.played.size
        center.update(w.input(items = listOf(q), digests = listOf(w.waiting(1, 1, 3 * hour))))
        assertTrue(sink.shown)
        assertEquals(Attention.CH_QUIET, sink.posts.last().channel)
        assertFalse(sink.posts.last().alert)
        assertEquals(sounds, sink.played.size)
    }

    @Test
    fun theSameDigestAgainOrATickDoesNotBringItBack() {
        val q = w.question()
        val input = w.input(items = listOf(q), digests = listOf(w.waiting(1, 1, 3 * hour)))
        center.update(input)
        sink.dismiss()
        val posts = sink.posts.size
        center.update(input)
        center.update(input, force = false)
        assertEquals(posts, sink.posts.size)
        assertFalse(sink.shown)
    }

    @Test
    fun theDigestPartLeavesWhenNothingRemainsAndTheNotificationGoesWithIt() {
        center.update(w.input(digests = listOf(w.waiting(1, 2, day))))
        assertTrue(sink.shown)
        assertEquals("2 waiting", sink.posts.last().content.title)
        center.update(w.input())
        assertFalse(sink.shown)
        assertEquals(1, sink.cancels)
    }

    @Test
    fun aDigestOfAMutedPortalAndInTheQuietWindowIsStillSilent() {
        val muted = listOf(w.portal(w.production, muted = true))
        center.update(w.input(portals = muted, digests = listOf(w.waiting(1, 2, day))))
        assertTrue(sink.played.isEmpty())
        assertFalse(sink.posts.last().alert)
    }
}
