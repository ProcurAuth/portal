package app.portalinbox.portal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ReferrerHintTest {
    @Test
    fun anAppGivesItsPackage() {
        assertEquals("com.android.chrome", ReferrerHint.of("android-app://com.android.chrome"))
        assertEquals("org.mozilla.firefox", ReferrerHint.of("android-app://org.mozilla.firefox/https/example.org"))
    }

    @Test
    fun aWebPageGivesItsHostOnly() {
        assertEquals("mail.example.org", ReferrerHint.of("https://Mail.Example.org/inbox?x=1#y"))
        assertEquals("example.org", ReferrerHint.of("http://example.org:8080/a"))
    }

    @Test
    fun nothingOrOddTextGivesNull() {
        assertNull(ReferrerHint.of(null))
        assertNull(ReferrerHint.of(""))
        assertNull(ReferrerHint.of("   "))
        assertNull(ReferrerHint.of("chrome"))
        assertNull(ReferrerHint.of("file:///sdcard/x"))
        assertNull(ReferrerHint.of("android-app://"))
        assertNull(ReferrerHint.of("android-app://not a package"))
        assertNull(ReferrerHint.of("android-app://com.evil‮.app"))
        assertNull(ReferrerHint.of("https://exa mple.org/"))
    }
}
