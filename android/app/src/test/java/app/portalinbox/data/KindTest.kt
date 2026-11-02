package app.portalinbox.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class KindTest {
    private fun frame(kind: String?, title: String? = "disk full"): JSONObject {
        val o = JSONObject()
            .put("t", "alert")
            .put("alert_id", "ab".repeat(16))
            .put("group", "ops")
            .put("group_name", "Operations")
            .put("created_at_ms", 1_700_000_000_000L)
        if (kind != null) o.put("kind", kind)
        if (title != null) o.put("title", title)
        return o
    }

    @Test
    fun theKindAlertIsKnown() {
        assertEquals("alert", Kind.normalize("alert"))
        assertTrue(Kind.isKnown(Kind.ALERT))
        val f = AlertFrame.parse(frame("alert"))
        assertEquals(Kind.ALERT, f.kind)
        assertEquals("disk full", f.title)
        assertEquals("ops", f.group)
        assertEquals("Operations", f.groupName)
        assertEquals(1_700_000_000_000L, f.createdAtMs)
    }

    @Test
    fun aFutureKindIsKeptAndNotDropped() {
        for (kind in listOf("question", "approval", "x_1")) {
            val f = AlertFrame.parse(frame(kind))
            assertEquals(kind, f.kind)
            assertFalse(Kind.isKnown(f.kind))
            assertEquals("disk full", f.title)
        }
    }

    @Test
    fun aBadOrMissingKindBecomesUnknownAndTheAlertIsKept() {
        for (kind in listOf("", "Alert", "ALERT", "a b", "1x", "x".repeat(33), "kind\n", "<b>")) {
            assertEquals(kind, Kind.UNKNOWN, AlertFrame.parse(frame(kind)).kind)
        }
        assertEquals(Kind.UNKNOWN, AlertFrame.parse(frame(null)).kind)
        assertEquals(Kind.UNKNOWN, AlertFrame.parse(frame("alert").put("kind", JSONObject.NULL)).kind)
        assertEquals(Kind.UNKNOWN, AlertFrame.parse(frame("alert").put("kind", 1)).kind)
        assertEquals(Kind.UNKNOWN, Kind.normalize(null))
    }

    @Test
    fun aMissingFieldOfTheDeliveryStillFails() {
        assertThrows(org.json.JSONException::class.java) { AlertFrame.parse(frame("alert", title = null)) }
        assertThrows(org.json.JSONException::class.java) { AlertFrame.parse(frame("alert").also { it.remove("alert_id") }) }
    }
}
