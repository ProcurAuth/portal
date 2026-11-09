package app.portalinbox.portal

import app.portalinbox.portal.PortalNaming.Check
import app.portalinbox.portal.PortalNaming.Reason
import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PortalNamingTest {
    private val none = emptyList<PortalIdentity>()

    private fun valid(raw: String, existing: List<PortalIdentity> = none) = validate(raw, existing) as Check.Valid
    private fun validate(raw: String, existing: List<PortalIdentity> = none) = PortalNaming.validate(raw, existing)
    private fun reason(raw: String, existing: List<PortalIdentity> = none) = (validate(raw, existing) as Check.Invalid).reason

    @Test
    fun aPlainNameIsValidAndKeepsItsSpelling() {
        val v = valid("Production")
        assertEquals("Production", v.nickname)
        assertEquals(PortalNaming.normalize("Production"), v.key)
        assertEquals("Client A", valid("Client A").nickname)
        assertEquals("Café", valid("Café").nickname)
    }

    @Test
    fun theNameIsRequired() {
        assertEquals(Reason.EMPTY, reason(""))
        assertEquals(Reason.EMPTY, reason("   "))
    }

    @Test
    fun spacesAtBothEndsAreRemoved() {
        assertEquals("Prod", valid("  Prod  ").nickname)
    }

    @Test
    fun lengthOneAndSixteenAreValidAndSeventeenIsNot() {
        assertEquals("A", valid("A").nickname)
        assertEquals("a".repeat(16), valid("a".repeat(16)).nickname)
        assertEquals(Reason.TOO_LONG, reason("a".repeat(17)))
    }

    @Test
    fun lengthCountsCharactersNotUtf16Units() {
        // 16 emoji are 32 UTF-16 units and 16 code points.
        val sixteen = "😀".repeat(16)
        assertEquals(sixteen, valid(sixteen).nickname)
        assertEquals(Reason.TOO_LONG, reason(sixteen + "😀"))
    }

    @Test
    fun controlCharactersAreRefused() {
        for (bad in listOf("Pr\u0000od", "Pr\nod", "Pr\tod", "Pr\u007Fod", "Pr\u0085od", "\u0007")) {
            assertEquals("for ${bad.map { it.code }}", Reason.HIDDEN_CHARACTERS, reason(bad))
        }
    }

    @Test
    fun bidiCharactersAreRefused() {
        for (cp in listOf(0x202A, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066, 0x2067, 0x2068, 0x2069, 0x200E, 0x200F, 0x061C)) {
            assertEquals("U+%04X".format(cp), Reason.HIDDEN_CHARACTERS, reason("Pr" + String(Character.toChars(cp)) + "od"))
        }
    }

    @Test
    fun zeroWidthAndInvisibleCharactersAreRefused() {
        for (cp in listOf(0x200B, 0x200C, 0x200D, 0x2060, 0xFEFF, 0x00AD, 0x3164, 0x2800, 0xFE0F)) {
            assertEquals("U+%04X".format(cp), Reason.HIDDEN_CHARACTERS, reason("Pr" + String(Character.toChars(cp)) + "od"))
        }
    }

    @Test
    fun spacesOtherThanThePlainSpaceAreRefusedAtTheEndsToo() {
        for (cp in listOf(0x00A0, 0x2003, 0x3000, 0x2028)) {
            assertEquals("U+%04X".format(cp), Reason.HIDDEN_CHARACTERS, reason("Prod" + String(Character.toChars(cp))))
            assertEquals("U+%04X".format(cp), Reason.HIDDEN_CHARACTERS, reason(String(Character.toChars(cp)) + "Prod"))
        }
    }

    @Test
    fun privateUseAndUnassignedCharactersAreRefused() {
        assertEquals(Reason.HIDDEN_CHARACTERS, reason("Prod"))
        assertEquals(Reason.HIDDEN_CHARACTERS, reason("Pr͸od"))
    }

    @Test
    fun aNameOfSeparatorsOnlyIsRefused() {
        assertEquals(Reason.ONLY_SEPARATORS, reason("---"))
        assertEquals(Reason.ONLY_SEPARATORS, reason("_ . -"))
    }

    @Test
    fun prodProdPr0dAndPrDashOdAreTheSameKey() {
        val keys = listOf("Prod", "prod", "Pr0d", "Pr-od", "PROD", "p r o d", "Pr_o.d").map { PortalNaming.normalize(it) }
        assertEquals(1, keys.toSet().size)
        assertEquals(keys.first(), valid("Prod").key)
    }

    @Test
    fun oneIAndLAreTheSameKey() {
        val keys = listOf("Il1", "lI1", "111", "lll", "iii").map { PortalNaming.normalize(it) }
        assertEquals(1, keys.toSet().size)
        assertEquals(PortalNaming.normalize("Alice"), PortalNaming.normalize("A1ice"))
        assertEquals(PortalNaming.normalize("Alice"), PortalNaming.normalize("AIice"))
    }

    @Test
    fun cyrillicAndFullWidthLookalikesFoldToLatin() {
        // Cyrillic a, o, e and r? "р" is the Cyrillic er, which looks like a Latin p.
        assertEquals(PortalNaming.normalize("Prod"), PortalNaming.normalize("рrоd"))
        assertEquals(PortalNaming.normalize("Prod"), PortalNaming.normalize("Ｐｒｏｄ"))
        assertEquals(PortalNaming.normalize("Cafe"), PortalNaming.normalize("Café"))
    }

    @Test
    fun differentNamesStayDifferent() {
        assertTrue(PortalNaming.normalize("Production") != PortalNaming.normalize("Staging"))
        assertTrue(PortalNaming.normalize("Client A") != PortalNaming.normalize("Client B"))
    }

    @Test
    fun aNameThatAnotherPortalHasIsTakenWhateverItsSpelling() {
        val existing = listOf(Portals.identity(1, "Production", 0))
        for (raw in listOf("Production", "production", "Pr0duction", "Pr-oduction", " PRODUCTION ")) {
            val c = validate(raw, existing) as Check.Invalid
            assertEquals(raw, Reason.TAKEN, c.reason)
            assertEquals(1L, c.conflict?.id)
        }
        assertTrue(validate("Staging", existing) is Check.Valid)
    }

    @Test
    fun renamingAPortalToItsOwnNameIsNotAConflict() {
        val existing = listOf(Portals.identity(1, "Production", 0), Portals.identity(2, "Staging", 1))
        assertTrue(PortalNaming.validate("PRODUCTION", existing, ignoreId = 1) is Check.Valid)
        assertTrue(PortalNaming.validate("Staging", existing, ignoreId = 1) is Check.Invalid)
    }

    @Test
    fun theRuleOfHiddenCharactersComesBeforeTheOthers() {
        // A hidden character in a too long name gives the hidden reason: the user must see that first.
        assertEquals(Reason.HIDDEN_CHARACTERS, reason("a".repeat(20) + "‮"))
    }

    @Test
    fun monogramIsTheFirstTwoLettersInUpperCase() {
        assertEquals("PR", PortalNaming.monogram("Production"))
        assertEquals("CL", PortalNaming.monogram("Client A"))
        assertEquals("H", PortalNaming.monogram("H"))
        assertEquals("CL", PortalNaming.monogram("  - client"))
        assertEquals("A1", PortalNaming.monogram("a1x"))
        assertEquals("É", PortalNaming.monogram("é"))
        assertEquals("#", PortalNaming.monogram("#"))
        assertEquals("😀", PortalNaming.monogram("😀😀"))
    }
}
