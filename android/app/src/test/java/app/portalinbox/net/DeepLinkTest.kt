package app.portalinbox.net

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DeepLinkTest {
    @Test
    fun onlyTheEnrollmentLinkMatches() {
        assertTrue(DeepLink.isEnroll("portal-inbox", "enroll"))
        assertFalse(DeepLink.isEnroll("portal-inbox", "add"))
        assertFalse(DeepLink.isEnroll("Portal-Inbox", "enroll"))
        assertFalse(DeepLink.isEnroll("https", "enroll"))
        assertFalse(DeepLink.isEnroll(null, null))
    }

    @Test
    fun theManifestDeclaresTheSameSchemeAndHost() {
        val manifest = File("src/main/AndroidManifest.xml").readText()
        val data = Regex("""<data\s+android:scheme="([^"]*)"\s+android:host="([^"]*)"\s*/>""").findAll(manifest).toList()
        assertEquals("one data element", 1, data.size)
        assertEquals(DeepLink.SCHEME, data[0].groupValues[1])
        assertEquals(DeepLink.HOST, data[0].groupValues[2])
        assertTrue(manifest.contains("android.intent.action.VIEW"))
        assertTrue(manifest.contains("android.intent.category.BROWSABLE"))
    }
}
