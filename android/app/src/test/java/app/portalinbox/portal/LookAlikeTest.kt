package app.portalinbox.portal

import app.portalinbox.portal.LookAlike.Finding
import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class LookAlikeTest {
    private val prod = Portals.known(1, "Production", 0, "https://portal.example.org")
    private val staging = Portals.known(2, "Staging", 1, "http://10.0.0.5:8080")
    private val existing = listOf(prod, staging)

    private fun check(address: String, keyNo: Int, nickname: String? = null, list: List<KnownPortal> = existing) =
        LookAlike.check(address, Portals.fp(keyNo), list, nickname)

    @Test
    fun noPortalGivesNone() {
        assertEquals(Finding.None, check("https://portal.example.org", 9, list = emptyList()))
    }

    @Test
    fun porta1AgainstPortalIsSimilar() {
        val f = check("https://porta1.example.org", 9) as Finding.SimilarAddress
        assertEquals(1L, f.portal.identity.id)
        assertEquals(0, f.distance) // equal after the folding of 1 and l
    }

    @Test
    fun otherConfusablesOfTheHost() {
        // 0 and o.
        assertTrue(check("https://p0rtal.example.org", 9) is Finding.SimilarAddress)
        // rn and m.
        val m = listOf(Portals.known(1, "Mail", 0, "https://mail.example.org"))
        assertTrue(check("https://rnail.example.org", 9, list = m) is Finding.SimilarAddress)
        // A swap of two letters is one edit.
        assertTrue(check("https://protal.example.org", 9) is Finding.SimilarAddress)
        // One letter more.
        assertTrue(check("https://portall.example.org", 9) is Finding.SimilarAddress)
    }

    @Test
    fun sameKeyWinsWhateverTheAddress() {
        val f = check("https://elsewhere.example.net", 1) as Finding.SameKey
        assertEquals(1L, f.portal.identity.id)
        // Also when the address is the same.
        assertTrue(check("https://portal.example.org", 1) is Finding.SameKey)
    }

    @Test
    fun sameKeyIsFoundWithTheCompactForm() {
        val compact = Fingerprint.compact(Portals.fp(2))
        assertTrue(LookAlike.check("https://other.example.net", compact, existing) is Finding.SameKey)
    }

    @Test
    fun sameAddressWithAnotherKey() {
        val f = check("https://portal.example.org", 9) as Finding.SameAddressOtherKey
        assertEquals(1L, f.portal.identity.id)
        // The scheme default port and the case of the host do not matter.
        assertTrue(check("https://PORTAL.example.org:443", 9) is Finding.SameAddressOtherKey)
        // The scheme is not part of the address: one host and one port serve one scheme.
        assertTrue(check("portal.example.org:443", 9) is Finding.SameAddressOtherKey)
    }

    @Test
    fun sameAddressNeedsTheSamePortAndPath() {
        assertTrue(check("http://10.0.0.5:8080", 9) is Finding.SameAddressOtherKey)
        // Same IP, other port: the same host, flagged as similar (distance 0), not as the same address.
        val f = check("http://10.0.0.5:9090", 9) as Finding.SimilarAddress
        assertEquals(0, f.distance)
        // Same host, other path (two portals behind one proxy).
        assertTrue(check("https://portal.example.org/staging", 9) is Finding.SimilarAddress)
    }

    @Test
    fun ipAddressesThatDifferAreNotSimilar() {
        assertEquals(Finding.None, check("http://10.0.0.6:8080", 9))
        assertEquals(Finding.None, check("http://10.0.0.55:8080", 9))
        assertEquals(Finding.None, check("http://192.168.1.10", 9))
    }

    @Test
    fun punycodeIsDecodedAndFolded() {
        // "pоrtal.example.org" with a Cyrillic o (U+043E) is "xn--prtal-...". Build it with the JDK so that no table is copied.
        val cyrillic = java.net.IDN.toASCII("pоrtal.example.org")
        assertTrue(cyrillic.startsWith("xn--"))
        val parts = LookAlike.parts("https://$cyrillic")!!
        assertEquals("pоrtal.example.org", parts.host)
        assertTrue(LookAlike.isInternational(cyrillic))
        assertFalse(LookAlike.isInternational("portal.example.org"))
        val f = check("https://$cyrillic", 9) as Finding.SimilarAddress
        assertEquals(1L, f.portal.identity.id)
        assertEquals(0, f.distance)
    }

    @Test
    fun displayHostDecodesPunycodeAndLowerCases() {
        assertEquals("bücher.example", LookAlike.displayHost(java.net.IDN.toASCII("bücher.example")))
        assertEquals("portal.example.org", LookAlike.displayHost("PORTAL.Example.ORG"))
    }

    @Test
    fun unrelatedNamesGiveNone() {
        assertEquals(Finding.None, check("https://billing.acme.io", 9))
        assertEquals(Finding.None, check("https://homelab.local:8443", 9))
        assertEquals(Finding.None, check("https://a.io", 9, list = listOf(Portals.known(1, "X", 0, "https://b.io"))))
    }

    @Test
    fun aShortHostNeedsAnExactFoldedMatch() {
        val short = listOf(Portals.known(1, "Short", 0, "https://x.co"))
        // 1 edit on 4 characters is too much freedom.
        assertEquals(Finding.None, check("https://y.co", 9, list = short))
        assertTrue(check("https://abc0.io", 9, list = listOf(Portals.known(1, "Short", 0, "https://abco.io"))) is Finding.SimilarAddress)
    }

    @Test
    fun theClosestPortalIsReported() {
        val list = listOf(
            Portals.known(1, "Far", 0, "https://portal.exampl.org"),
            Portals.known(2, "Near", 1, "https://portal.example.org"),
        )
        val f = check("https://portal.example.orgs", 9, list = list) as Finding.SimilarAddress
        assertEquals(2L, f.portal.identity.id)
    }

    @Test
    fun similarNamesAreFound() {
        val f = check("https://billing.acme.io", 9, nickname = "Pr0duction") as Finding.SimilarName
        assertEquals(1L, f.portal.identity.id)
        assertTrue(check("https://billing.acme.io", 9, nickname = "Productio") is Finding.SimilarName)
        assertEquals(Finding.None, check("https://billing.acme.io", 9, nickname = "Homelab"))
        assertEquals(Finding.None, check("https://billing.acme.io", 9, nickname = null))
        assertEquals(Finding.None, check("https://billing.acme.io", 9, nickname = "  "))
    }

    @Test
    fun shortNamesNeedAnExactFoldedMatch() {
        val list = listOf(Portals.known(1, "Dev", 0, "https://dev.example.org"))
        assertEquals(Finding.None, check("https://billing.acme.io", 9, nickname = "Dex", list = list))
        assertTrue(check("https://billing.acme.io", 9, nickname = "DEV", list = list) is Finding.SimilarName)
    }

    @Test
    fun theStrongestFindingWinsOverAWeakerOneOfAnotherPortal() {
        // The name looks like Production, the address is the address of Staging with another key.
        val f = check("http://10.0.0.5:8080", 9, nickname = "Production")
        assertTrue(f is Finding.SameAddressOtherKey)
        assertEquals(2L, (f as Finding.SameAddressOtherKey).portal.identity.id)
    }

    @Test
    fun anUnparseableAddressIsNotAFindingByItself() {
        assertNull(LookAlike.parts("ftp://x"))
        assertNull(LookAlike.parts(""))
        assertEquals(Finding.None, check("::::", 9))
    }

    @Test
    fun changedPositionsMarksTheDifferentLetter() {
        assertEquals(setOf(5), Confusables.changedPositions("porta1.example.org", "portal.example.org"))
        assertEquals(emptySet<Int>(), Confusables.changedPositions("same", "same"))
        // An added letter.
        // One of the two equal letters is the added one: the place is 5 or 6.
        assertEquals(1, Confusables.changedPositions("portall", "portal").size)
        assertTrue(Confusables.changedPositions("portall", "portal").single() in 5..6)
        // A removed letter marks nothing in the new text.
        assertEquals(emptySet<Int>(), Confusables.changedPositions("portl", "portal"))
    }
}
