package app.portalinbox.portal

import app.portalinbox.testsupport.Portals
import app.portalinbox.testsupport.ReferenceFingerprint
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class FingerprintTest {
    private fun of(key: ByteArray) = Fingerprint.of(key, ReferenceFingerprint)

    /**
     * The golden values of `inbox-core` (`enroll.rs`, test `fingerprint_is_eighty_bits_of_sha256_in_groups_of_four`) and of the
     * bridge (`bridge.rs`, test `fingerprint_of_a_key_has_the_golden_value_and_refuses_a_wrong_length`). The Rust function is the
     * real one. The fake of the test must give the same text.
     */
    @Test
    fun goldenVectorsOfInboxCore() {
        assertEquals("6668 7aad f862 bd77 6c8f", of(ByteArray(32)))
        assertEquals("9361 3343 0b8c 6a2b c988", of(Portals.key(50323)))
        assertEquals("9361 3343 acd6 e90c 30a9", of(Portals.key(54260)))
    }

    @Test
    fun theTwoKeysThatShareTheirFirst32BitsAreToldApart() {
        val a = of(Portals.key(50323))
        val b = of(Portals.key(54260))
        assertEquals(Fingerprint.short(a, 2), Fingerprint.short(b, 2))
        assertFalse(Fingerprint.same(a, b))
    }

    @Test
    fun displayFormHasFiveGroupsOfFour() {
        val fp = of(Portals.key(1))
        assertEquals(5, fp.split(' ').size)
        assertTrue(fp.split(' ').all { it.length == 4 })
        assertTrue(Fingerprint.isDisplay(fp))
        assertEquals(24, fp.length)
    }

    @Test
    fun compactFormHasTwentyCharactersAndRoundTrips() {
        val fp = of(ByteArray(32))
        val compact = Fingerprint.compactOf(ByteArray(32), ReferenceFingerprint)
        assertEquals("66687aadf862bd776c8f", compact)
        assertEquals(20, compact.length)
        assertTrue(Fingerprint.isCompact(compact))
        assertEquals(fp, Fingerprint.display(compact))
        assertEquals(compact, Fingerprint.compact(fp))
    }

    @Test
    fun shortGivesTheFirstGroups() {
        val fp = "6668 7aad f862 bd77 6c8f"
        assertEquals("6668", Fingerprint.short(fp, 1))
        assertEquals("6668 7aad", Fingerprint.short(fp, 2))
        assertEquals("6668 7aad f862", Fingerprint.short(fp, 3))
        assertEquals(fp, Fingerprint.short(fp, 5))
        // The compact form gives the same.
        assertEquals("6668 7aad", Fingerprint.short("66687aadf862bd776c8f", 2))
        for (bad in listOf(0, 6, -1)) {
            try {
                Fingerprint.short(fp, bad)
                fail("groups $bad must be refused")
            } catch (e: IllegalArgumentException) {
            }
        }
    }

    @Test
    fun aKeyOfTheWrongLengthIsRefused() {
        for (size in listOf(0, 1, 31, 33, 64)) {
            try {
                Fingerprint.of(ByteArray(size), ReferenceFingerprint)
                fail("a key of $size bytes must be refused")
            } catch (e: IllegalArgumentException) {
                assertTrue(e.message!!.contains("32"))
            }
        }
    }

    @Test
    fun aSourceThatGivesAnotherFormIsRefused() {
        for (bad in listOf("", "6668 7aad f862 bd77", "6668 7aad f862 bd77 6c8", "6668-7aad-f862-bd77-6c8f", "6668 7AAD f862 bd77 6c8f", "zzzz 7aad f862 bd77 6c8f")) {
            try {
                Fingerprint.of(ByteArray(32)) { bad }
                fail("'$bad' must be refused")
            } catch (e: IllegalStateException) {
            }
        }
    }

    @Test
    fun parseReadsWhatAPersonTypes() {
        assertEquals("66687aadf862bd776c8f", Fingerprint.parse("6668 7aad f862 bd77 6c8f"))
        assertEquals("66687aadf862bd776c8f", Fingerprint.parse("  6668 7AAD F862 BD77 6C8F\n"))
        assertEquals("66687aadf862bd776c8f", Fingerprint.parse("66687aadf862bd776c8f"))
        assertNull(Fingerprint.parse(""))
        assertNull(Fingerprint.parse("6668 7aad"))
        assertNull(Fingerprint.parse("6668 7aad f862 bd77 6c8g"))
        assertNull(Fingerprint.parse("6668 7aad f862 bd77 6c8f0"))
    }

    @Test
    fun sameComparesWhateverTheForm() {
        assertTrue(Fingerprint.same("6668 7aad f862 bd77 6c8f", "66687AADF862BD776C8F"))
        assertFalse(Fingerprint.same("6668 7aad f862 bd77 6c8f", "9361 3343 0b8c 6a2b c988"))
        assertFalse(Fingerprint.same("nope", "nope"))
    }

    @Test
    fun displayRefusesATextThatIsNotCompact() {
        for (bad in listOf("", "6668 7aad f862 bd77 6c8f", "66687aadf862bd776c8")) {
            try {
                Fingerprint.display(bad)
                fail("'$bad' must be refused")
            } catch (e: IllegalArgumentException) {
            }
        }
    }
}
