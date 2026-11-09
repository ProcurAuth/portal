package app.portalinbox.portal

import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PortalIdentityTest {
    @Test
    fun theIdentityComesFromTheLocalFieldsOnly() {
        val server = Portals.server(id = 7, nickname = "Production", color = 4, keyNo = 3)
        val id = PortalIdentity.of(server)
        assertEquals(7L, id.id)
        assertEquals("Production", id.nickname)
        assertEquals(4, id.colorIndex)
        assertEquals("PR", id.monogram)
        assertEquals(Portals.fp(3), id.fingerprint)
        // What the portal says about itself is not in the identity.
        assertFalse(id.toString().contains(server.reportedName))
    }

    @Test
    fun shortFingerprintHasTheFirstGroups() {
        val id = PortalIdentity.of(Portals.server(keyNo = 1))
        assertEquals(id.fingerprint.split(' ').take(2).joinToString(" "), id.shortFingerprint())
        assertEquals(id.fingerprint, id.shortFingerprint(5))
    }

    @Test
    fun colorFollowsTheTheme() {
        val id = PortalIdentity.of(Portals.server(color = 2))
        assertEquals(PortalPalette.dark(2), id.color(night = true))
        assertEquals(PortalPalette.light(2), id.color(night = false))
        assertEquals(PortalPalette.onColor(true), id.onColor(true))
    }

    @Test
    fun onTextNamesThePortalForAButton() {
        assertEquals("on Production", PortalIdentity.of(Portals.server()).onText)
    }

    @Test
    fun newMarkLastsTwentyFourHours() {
        val added = 1_000_000L
        assertTrue(PortalIdentity.isNew(added, added))
        assertTrue(PortalIdentity.isNew(added, added + 24 * 3_600_000L - 1))
        assertFalse(PortalIdentity.isNew(added, added + 24 * 3_600_000L))
        // The phone clock went back: still new.
        assertTrue(PortalIdentity.isNew(added, added - 5_000))
    }

    @Test
    fun knownPortalCarriesTheAddressAndTheCompactKey() {
        val k = KnownPortal.of(Portals.server(keyNo = 5, address = "https://x.example.org"))
        assertEquals("https://x.example.org", k.address)
        assertEquals(Fingerprint.compact(Portals.fp(5)), k.keyFp)
        assertEquals(20, k.keyFp.length)
    }
}
