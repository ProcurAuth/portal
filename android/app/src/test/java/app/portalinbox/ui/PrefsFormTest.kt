package app.portalinbox.ui

import app.portalinbox.proto.DigestMode
import app.portalinbox.proto.DigestPrefs
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.QuietPrefs
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PrefsFormTest {
    private val phone = "Europe/Paris"
    private val full = Prefs(QuietPrefs("22:00", "07:00", "Europe/Paris", true), DigestPrefs(DigestMode.DAILY, "09:00", "Europe/Paris", null))

    @Test
    fun theFormShowsTheValuesThatApply() {
        val d = PrefsForm.draftOf(full, "Asia/Tokyo")
        assertTrue(d.quietOn)
        assertEquals("22:00", d.quietFrom)
        assertEquals("Europe/Paris", d.quietTz)
        assertTrue(d.highRings)
        assertEquals(DigestMode.DAILY, d.digestMode)
        assertFalse(d.digestUnset)
    }

    @Test
    fun withoutPrefsTheFormOffersTheDefaultsInTheZoneOfThePhone() {
        val d = PrefsForm.draftOf(Prefs.EMPTY, "Asia/Tokyo")
        assertFalse(d.quietOn)
        assertEquals("22:00", d.quietFrom)
        assertEquals("07:00", d.quietTo)
        assertEquals("Asia/Tokyo", d.quietTz)
        assertTrue(d.digestUnset)
    }

    @Test
    fun aDraftReadsBackAsTheSamePrefs() {
        assertEquals(PrefsForm.Result.Ok(full), PrefsForm.toPrefs(PrefsForm.draftOf(full, phone)))
        assertEquals(PrefsForm.Result.Ok(Prefs.EMPTY), PrefsForm.toPrefs(PrefsForm.draftOf(Prefs.EMPTY, phone)))
    }

    @Test
    fun aWindowWithTheSameStartAndEndIsRefusedNotShifted() {
        val d = PrefsForm.draftOf(full, phone).copy(quietTo = "22:00")
        assertTrue(PrefsForm.toPrefs(d) is PrefsForm.Result.Refused)
        // With the quiet hours off the same times are not a problem.
        assertTrue(PrefsForm.toPrefs(d.copy(quietOn = false)) is PrefsForm.Result.Ok)
    }

    @Test
    fun theDigestFollowsItsMode() {
        val d = PrefsForm.draftOf(full, phone)
        val off = (PrefsForm.toPrefs(d.copy(digestMode = DigestMode.OFF)) as PrefsForm.Result.Ok).prefs.digest
        assertEquals(DigestPrefs(DigestMode.OFF, null, null, null), off)
        val every = (PrefsForm.toPrefs(d.copy(digestMode = DigestMode.EVERY, everyH = 4)) as PrefsForm.Result.Ok).prefs.digest
        assertEquals(DigestPrefs(DigestMode.EVERY, null, null, 4), every)
        val daily = (PrefsForm.toPrefs(d.copy(digestAt = "08:30")) as PrefsForm.Result.Ok).prefs.digest
        assertEquals(DigestPrefs(DigestMode.DAILY, "08:30", "Europe/Paris", null), daily)
    }

    @Test
    fun theEveryHoursStayInTheRangeOfTheProtocol() {
        val d = PrefsForm.draftOf(full, phone).copy(digestMode = DigestMode.EVERY)
        assertTrue(PrefsForm.toPrefs(d.copy(everyH = 1)) is PrefsForm.Result.Ok)
        assertTrue(PrefsForm.toPrefs(d.copy(everyH = 168)) is PrefsForm.Result.Ok)
        assertTrue(PrefsForm.toPrefs(d.copy(everyH = 0)) is PrefsForm.Result.Refused)
        assertTrue(PrefsForm.toPrefs(d.copy(everyH = 169)) is PrefsForm.Result.Refused)
    }

    @Test
    fun aDigestThatTheOwnerNeverChoseStaysUnset() {
        val d = PrefsForm.draftOf(Prefs(QuietPrefs("22:00", "07:00", phone, false), null), phone)
        val prefs = (PrefsForm.toPrefs(d) as PrefsForm.Result.Ok).prefs
        assertNull(prefs.digest)
    }

    @Test
    fun everyValueThatTheFormMakesIsAValidFrame() {
        val d = PrefsForm.draftOf(full, phone)
        for (draft in listOf(d, d.copy(digestMode = DigestMode.OFF), d.copy(digestMode = DigestMode.EVERY, everyH = 168), d.copy(quietOn = false))) {
            val prefs = (PrefsForm.toPrefs(draft) as PrefsForm.Result.Ok).prefs
            app.portalinbox.proto.SetPrefsFrame(prefs).validate()
        }
    }

    @Test
    fun theSummaryReadsInWords() {
        assertEquals("Quiet 22:00 to 07:00 · Daily summary at 09:00", PrefsForm.summary(full))
        assertEquals("No quiet hours · Summary: server default", PrefsForm.summary(Prefs.EMPTY))
        assertEquals("No quiet hours · No summary", PrefsForm.summary(Prefs(null, DigestPrefs(DigestMode.OFF, null, null, null))))
        assertEquals("No quiet hours · Summary every 4 h", PrefsForm.summary(Prefs(null, DigestPrefs(DigestMode.EVERY, null, null, 4))))
    }

    @Test
    fun theTimeIsReadAndWrittenAsHhmm() {
        assertEquals(7, PrefsForm.hour("07:30"))
        assertEquals(30, PrefsForm.minute("07:30"))
        assertEquals("07:05", PrefsForm.hhmm(7, 5))
    }

    // ------------------------------------------------------------ status and texts of the settings screen

    @Test
    fun theStatusFollowsThePendingEditTheLinkAndTheRefusal() {
        assertEquals(PrefsForm.Status.CONFIRMED, PrefsForm.status(pending = false, connected = true, refused = false))
        assertEquals(PrefsForm.Status.WAITING, PrefsForm.status(pending = true, connected = true, refused = false))
        assertEquals(PrefsForm.Status.WAITING_OFFLINE, PrefsForm.status(pending = true, connected = false, refused = false))
        assertEquals(PrefsForm.Status.REFUSED, PrefsForm.status(pending = false, connected = true, refused = true))
        // An edit after a refusal is pending again.
        assertEquals(PrefsForm.Status.WAITING, PrefsForm.status(pending = true, connected = true, refused = true))
    }

    @Test
    fun theRefusalOfThePortalIsOneSafeShortLine() {
        assertEquals("prefs.quiet.tz: expected a zone, got Mars/Base", PrefsForm.refusalLine("prefs.quiet.tz: expected a zone,\n  got   Mars/Base \n"))
        assertFalse(PrefsForm.refusalLine("ops\u202Eagent").contains('\u202E'))
        val long = PrefsForm.refusalLine("x".repeat(500))
        assertEquals(160, long.length)
        assertTrue(long.endsWith("…"))
    }

    @Test
    fun theRowsOfTheSettingsScreenSayWhatApplies() {
        val q = QuietPrefs("22:00", "07:00", "Europe/Paris", true)
        assertEquals("Quiet from 22:00 to 07:00", PrefsForm.quietTitle(q))
        assertEquals("Europe/Paris. No sound. Items still arrive.", PrefsForm.quietDetail(q))
        assertEquals("Daily at 09:00", PrefsForm.digestTitle(full.digest))
        assertEquals("Europe/Paris", PrefsForm.digestDetail(full.digest))
        assertEquals("Every 6 hours", PrefsForm.digestTitle(DigestPrefs(DigestMode.EVERY, null, null, 6)))
        assertEquals("Every hour", PrefsForm.digestTitle(DigestPrefs(DigestMode.EVERY, null, null, 1)))
        assertNull(PrefsForm.digestDetail(DigestPrefs(DigestMode.EVERY, null, null, 6)))
        assertEquals("No summary", PrefsForm.digestTitle(DigestPrefs(DigestMode.OFF, null, null, null)))
        assertEquals("Portal default", PrefsForm.digestTitle(null))
    }
}
