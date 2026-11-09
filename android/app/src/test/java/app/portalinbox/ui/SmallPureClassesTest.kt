package app.portalinbox.ui

import app.portalinbox.data.ClockOffsets
import app.portalinbox.proto.ExplanationFormat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId

class AgesTest {
    private val now = 1_788_000_000_000L

    @Test
    fun agoUsesPlainWords() {
        assertEquals("just now", Ages.ago(now - 10_000, now))
        assertEquals("3 min ago", Ages.ago(now - 180_000, now))
        assertEquals("5 h ago", Ages.ago(now - 5 * 3_600_000L, now))
        assertEquals("yesterday", Ages.ago(now - 30 * 3_600_000L, now))
        assertEquals("6 d ago", Ages.ago(now - 6 * 86_400_000L, now))
    }

    @Test
    fun aTimeInTheFutureCountsAsNow() {
        assertEquals("just now", Ages.ago(now + 60_000, now))
        assertEquals("<1 min", Ages.short(now + 60_000, now))
    }

    @Test
    fun longAgeNamesTheUnit() {
        assertEquals("6 days", Ages.long(6 * 86_400_000L))
        assertEquals("1 day", Ages.long(86_400_000L + 5))
        assertEquals("5 hours", Ages.long(5 * 3_600_000L))
        assertEquals("1 minute", Ages.long(10_000))
    }

    @Test
    fun timesUseTheZoneGiven() {
        assertEquals("2026-08-29 10:40", Ages.full(now, ZoneId.of("UTC")))
        assertEquals("08-29 12:40", Ages.dayTime(now, ZoneId.of("Europe/Paris")))
    }
}

class ClockOffsetsTest {
    @Test
    fun theOffsetMovesTheNowOfAServer() {
        val c = ClockOffsets()
        assertEquals(1000L, c.now(1, 1000L))
        c.update(1, serverTimeMs = 5_000, phoneNowMs = 1_000)
        assertEquals(4_000L, c.offset(1))
        assertEquals(12_000L, c.now(1, 8_000))
        assertEquals(8_000L, c.now(2, 8_000))
        c.forget(1)
        assertNull(c.offset(1))
    }
}

class BiometricPolicyTest {
    @Test
    fun api28And29UseAWeakBiometricWithTheCredential() {
        assertEquals(BiometricPolicy.Allowed.WEAK_OR_CREDENTIAL, BiometricPolicy.allowed(28))
        assertEquals(BiometricPolicy.Allowed.WEAK_OR_CREDENTIAL, BiometricPolicy.allowed(29))
        for (sdk in listOf(26, 27, 30, 33, 35)) assertEquals(BiometricPolicy.Allowed.STRONG_OR_CREDENTIAL, BiometricPolicy.allowed(sdk))
    }

    @Test
    fun thePromptNamesThePortalInTheTitleAndShowsTheCodeTheServiceAndTheShortHashOnly() {
        assertEquals("Approve on Production", BiometricPolicy.title("Production"))
        assertEquals("K7QF · datastore · sha256:3c1f9a02", BiometricPolicy.subtitle("K7QF", "datastore", "3c1f9a02"))
        assertEquals("K7QF · datastore", BiometricPolicy.subtitle("K7QF", "datastore", null))
    }

    @Test
    fun aHiddenCharacterInTheNicknameOrTheCodeShowsAsAMark() {
        assertEquals("Approve on Pr<U+202E>od", BiometricPolicy.title("Pr\u202Eod"))
        assertEquals("K7<U+202E>QF · datastore", BiometricPolicy.subtitle("K7\u202EQF", "datastore", null))
    }
}

class ExplanationTextTest {
    @Test
    fun noExplanationShowsTheAbsentLine() {
        assertEquals(ExplanationText.View.Absent, ExplanationText.view(null, null, false))
        assertEquals(ExplanationText.View.Absent, ExplanationText.view("  \n", null, false))
        assertEquals("No explanation given", ExplanationText.ABSENT)
    }

    @Test
    fun theFixedSentenceIsExact() {
        assertEquals("This explanation does not change what will run.", ExplanationText.FIXED_SENTENCE)
    }

    @Test
    fun aShortPlainTextShowsWhole() {
        assertEquals(ExplanationText.View.Plain("short", false), ExplanationText.view("short", null, false))
    }

    @Test
    fun aLongPlainTextFoldsAt400CharactersUntilShowAll() {
        val text = "x".repeat(401)
        val folded = ExplanationText.view(text, ExplanationFormat.PLAIN, expanded = false) as ExplanationText.View.Plain
        assertEquals(400, folded.shown.length)
        assertTrue(folded.folded)
        val all = ExplanationText.view(text, null, expanded = true) as ExplanationText.View.Plain
        assertEquals(text, all.shown)
        assertFalse(all.folded)
        assertFalse((ExplanationText.view("x".repeat(400), null, false) as ExplanationText.View.Plain).folded)
    }

    @Test
    fun theFoldNeverCutsInsideAPair() {
        val text = "😀".repeat(401)
        val v = ExplanationText.fold(text, false)
        assertEquals(800, v.shown.length)
        assertTrue(v.folded)
    }

    @Test
    fun markdownShowsAsMarkdownUnlessPlainTextIsAsked() {
        assertEquals(ExplanationText.View.Markdown("# t"), ExplanationText.view("# t", ExplanationFormat.MARKDOWN, false))
        assertEquals(ExplanationText.View.Plain("# t", false), ExplanationText.view("# t", ExplanationFormat.MARKDOWN, false, rawMarkdown = true))
    }

    @Test
    fun aPlainExplanationIsQuoted() {
        assertEquals("“hi”", ExplanationText.quoted("hi"))
    }
}
