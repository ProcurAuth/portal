package app.portalinbox.alert

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.Urgency
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.Portals
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.LocalDateTime
import java.time.ZoneId

class QuietHoursTest {
    private val paris = "Europe/Paris"
    private val night = QuietPrefs("22:00", "07:00", paris, highRings = false)
    private val loud = QuietPrefs("22:00", "07:00", paris, highRings = true)
    private val day = QuietPrefs("13:00", "14:30", paris, highRings = false)

    /** The instant of a local time in Paris on 2026-11-08 (winter time, UTC+1). */
    private fun at(h: Int, m: Int, date: String = "2026-11-08"): Long =
        LocalDateTime.parse("${date}T%02d:%02d:00".format(h, m)).atZone(ZoneId.of(paris)).toInstant().toEpochMilli()

    // ------------------------------------------------------------ the window

    @Test
    fun aWindowThatCrossesMidnightIsActiveOnBothSides() {
        assertTrue(QuietHours.active(night, at(22, 0)))
        assertTrue(QuietHours.active(night, at(23, 59)))
        assertTrue(QuietHours.active(night, at(0, 0)))
        assertTrue(QuietHours.active(night, at(6, 59)))
        assertFalse(QuietHours.active(night, at(7, 0)))
        assertFalse(QuietHours.active(night, at(12, 0)))
        assertFalse(QuietHours.active(night, at(21, 59)))
    }

    @Test
    fun aWindowInsideOneDayHasItsStartAndNotItsEnd() {
        assertFalse(QuietHours.active(day, at(12, 59)))
        assertTrue(QuietHours.active(day, at(13, 0)))
        assertTrue(QuietHours.active(day, at(14, 29)))
        assertFalse(QuietHours.active(day, at(14, 30)))
    }

    @Test
    fun theZoneOfThePrefsDecidesNotTheZoneOfThePhone() {
        // 22:30 in Paris is 21:30 UTC and 16:30 in New York.
        val ny = night.copy(tz = "America/New_York")
        val parisTime = at(22, 30)
        assertTrue(QuietHours.active(night, parisTime))
        assertFalse(QuietHours.active(ny, parisTime))
    }

    @Test
    fun theSummerTimeMovesTheWindowWithTheZone() {
        // In July Paris is UTC+2: 22:30 local is 20:30 UTC.
        val july = LocalDateTime.parse("2026-07-08T22:30:00").atZone(ZoneId.of(paris)).toInstant().toEpochMilli()
        assertTrue(QuietHours.active(night, july))
        assertFalse(QuietHours.active(night.copy(tz = "UTC"), july))
    }

    @Test
    fun aZoneThatThePhoneDoesNotKnowNeverSilencesASound() {
        assertFalse(QuietHours.active(night.copy(tz = "Nowhere/Land"), at(23, 0)))
    }

    @Test
    fun noPrefsMeansNoQuietHours() {
        assertFalse(QuietHours.suppressesSound(null, Urgency.NORMAL, at(23, 0)))
    }

    // ------------------------------------------------------------ the sound

    @Test
    fun thePhoneSuppressesTheSoundInTheWindowOnly() {
        assertTrue(QuietHours.suppressesSound(night, Urgency.NORMAL, at(23, 0)))
        assertFalse(QuietHours.suppressesSound(night, Urgency.NORMAL, at(9, 0)))
    }

    @Test
    fun anUrgentRequestRingsOnlyWhenThePrefsSayHighRings() {
        assertTrue(QuietHours.suppressesSound(night, Urgency.HIGH, at(23, 0)))
        assertFalse(QuietHours.suppressesSound(loud, Urgency.HIGH, at(23, 0)))
        // A normal request stays silent even with high_rings.
        assertTrue(QuietHours.suppressesSound(loud, Urgency.NORMAL, at(23, 0)))
    }

    @Test
    fun silentFollowsTheFlagOfTheServerTheUrgencyAndTheWindow() {
        // The server marked the request quiet.
        assertTrue(QuietHours.silent(true, Urgency.NORMAL, null, at(9, 0)))
        // Low urgency never rings.
        assertTrue(QuietHours.silent(false, Urgency.LOW, loud, at(9, 0)))
        // The phone sees the window even when the frame has no flag (a late notification).
        assertTrue(QuietHours.silent(false, Urgency.NORMAL, night, at(23, 0)))
        assertFalse(QuietHours.silent(false, Urgency.NORMAL, night, at(9, 0)))
        // An urgent request rings with high_rings, even if the server marked it quiet.
        assertFalse(QuietHours.silent(true, Urgency.HIGH, loud, at(23, 0)))
        // Without high_rings the same request is silent.
        assertTrue(QuietHours.silent(true, Urgency.HIGH, night, at(23, 0)))
        assertTrue(QuietHours.silent(true, Urgency.HIGH, null, at(23, 0)))
    }

    @Test
    fun theWindowReadsInWords() {
        assertEquals("22:00 to 07:00", QuietHours.describe(night))
    }

    // ------------------------------------------------------------ the one notification

    private val server = 1L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { 1_788_000_100_000L })
    private val identity = Portals.identity(1, "Production", 0)

    private fun approval(urgency: Urgency): Item {
        engine.onRequest(server, Make.approval(Make.hex(0xb), Make.hex(1)) { it.put("urgency", urgency.wire) })
        return repo.item(server, Make.hex(0xb))!!
    }

    private fun decide(item: Item, quiet: QuietPrefs?, nowMs: Long): Pair<Attention.Aggregate, Attention.Decision> {
        val a = Attention.compute(
            Attention.Input(listOf(Attention.Portal(identity, false, false, quiet, 0L)), listOf(item), emptyList(), emptyList(), nowMs),
        )
        return a to Attention.decide(emptyMap(), a)
    }

    @Test
    fun aRequestInTheWindowPostsOnTheQuietChannelWithoutSound() {
        val (a, d) = decide(approval(Urgency.NORMAL), night, at(23, 0))
        assertFalse(d.alert)
        assertEquals(Attention.Sound.NONE, d.sound)
        val plan = Attention.plan(a, d, false, false)
        assertEquals(Attention.CH_QUIET, plan.channel)
        // Quiet is only about the sound: the notification still shows the request, and the single approval keeps its action.
        assertEquals("1 to answer", plan.content.title)
        assertEquals(1L to Make.hex(0xb), plan.decline)
    }

    @Test
    fun aRequestOutsideTheWindowAlerts() {
        val (a, d) = decide(approval(Urgency.NORMAL), night, at(9, 0))
        assertTrue(d.alert)
        assertEquals(Attention.CH_ATTENTION, Attention.plan(a, d, false, false).channel)
    }

    @Test
    fun anUrgentRequestRingsInTheWindowWhenTheOwnerAllowsIt() {
        val i = approval(Urgency.HIGH)
        assertTrue(decide(i, loud, at(23, 0)).second.alert)
        assertFalse(decide(i, night, at(23, 0)).second.alert)
    }

    @Test
    fun withoutPrefsOnlyTheFlagOfTheFrameDecides() {
        assertTrue(decide(approval(Urgency.NORMAL), null, at(23, 0)).second.alert)
    }
}
