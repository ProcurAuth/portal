package app.portalinbox.portal

import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PortalLabelsTest {
    private val id = Portals.identity(1, "Production", 0)

    @Test
    fun theHostLosesTheSchemeThePortAndThePath() {
        assertEquals("portal.example.org", PortalLabels.hostOf("https://portal.example.org"))
        assertEquals("portal.example.org", PortalLabels.hostOf("https://portal.example.org:8443/inbox?x=1"))
        assertEquals("portal.example.org", PortalLabels.hostOf("wss://user@portal.example.org:443"))
        assertEquals("portal.example.org", PortalLabels.hostOf("portal.example.org"))
        assertEquals("[2001:db8::1]", PortalLabels.hostOf("https://[2001:db8::1]:8443/"))
    }

    @Test
    fun theBannerShowsTheHostTheLoginAndTheFirstTwoGroupsOfTheKey() {
        val server = Portals.server(address = "https://portal.example.org:8443")
        assertEquals("portal.example.org · alice", PortalLabels.hostAndLogin(server))
        assertEquals("key " + id.fingerprint.split(" ").take(2).joinToString(" "), PortalLabels.bannerKey(id))
    }

    @Test
    fun theLoginOfThePortalIsEscaped() {
        val server = Portals.server().copy(userName = "al‮ice")
        assertTrue(PortalLabels.hostAndLogin(server).endsWith("al<U+202E>ice"))
    }

    @Test
    fun theFooterShowsTheNicknameAndAllFiveGroups() {
        val footer = PortalLabels.footer(id)
        assertTrue(footer.startsWith("Production · key "))
        assertEquals(5, footer.substringAfter("key ").split(" ").size)
        assertEquals(id.fingerprint, footer.substringAfter("key "))
    }

    @Test
    fun theClipboardLabelNamesThePortal() {
        assertEquals("Production: command", PortalLabels.clipboardLabel(id))
    }
}
