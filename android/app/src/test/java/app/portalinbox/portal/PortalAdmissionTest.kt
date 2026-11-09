package app.portalinbox.portal

import app.portalinbox.portal.PortalAdmission.Decision
import app.portalinbox.portal.PortalAdmission.Refusal
import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PortalAdmissionTest {
    private fun portals(n: Int) = (1..n).map { Portals.known(it.toLong(), "Portal $it", it - 1, "https://p$it.example.org") }

    private fun check(existing: List<KnownPortal>, keyNo: Int, name: String, color: Int? = null) =
        PortalAdmission.check(existing, Portals.fp(keyNo), name, color)

    @Test
    fun theFirstPortalGetsTheFirstColor() {
        val d = check(emptyList(), 1, "Production") as Decision.Admit
        assertEquals("Production", d.nickname)
        assertEquals(0, d.colorIndex)
        assertEquals(PortalNaming.normalize("Production"), d.nicknameKey)
    }

    @Test
    fun theNextColorIsTheFirstFree() {
        val d = check(portals(3), 99, "Staging") as Decision.Admit
        assertEquals(3, d.colorIndex)
    }

    @Test
    fun aSecondEnrollmentOfTheSameKeyIsRefusedWhateverTheAddressOrName() {
        val r = (check(portals(2), 2, "Another name") as Decision.Refuse).refusal as Refusal.DuplicateKey
        assertEquals(2L, r.portal.identity.id)
    }

    @Test
    fun theKeyMayBeGivenInAnyForm() {
        val existing = portals(1)
        val compact = Fingerprint.compact(Portals.fp(1))
        val d = PortalAdmission.check(existing, compact, "Other", null) as Decision.Refuse
        assertTrue(d.refusal is Refusal.DuplicateKey)
    }

    @Test
    fun theTenthPortalIsAcceptedAndTheEleventhIsRefused() {
        val ten = portals(9)
        val tenth = check(ten, 100, "The tenth") as Decision.Admit
        assertEquals(9, tenth.colorIndex)
        val full = portals(10)
        assertEquals(Decision.Refuse(Refusal.CapReached), check(full, 101, "The eleventh"))
        assertEquals(10, PortalAdmission.MAX_PORTALS)
    }

    @Test
    fun theDuplicateKeyIsReportedBeforeTheCap() {
        val full = portals(10)
        assertTrue((check(full, 5, "X") as Decision.Refuse).refusal is Refusal.DuplicateKey)
    }

    @Test
    fun aBadOrTakenNameIsRefusedWithItsReason() {
        val existing = portals(1)
        val empty = ((check(existing, 50, "  ") as Decision.Refuse).refusal as Refusal.BadName).invalid
        assertEquals(PortalNaming.Reason.EMPTY, empty.reason)
        val taken = ((check(existing, 50, "p0rtal 1") as Decision.Refuse).refusal as Refusal.BadName).invalid
        assertEquals(PortalNaming.Reason.TAKEN, taken.reason)
        val hidden = ((check(existing, 50, "A‮B") as Decision.Refuse).refusal as Refusal.BadName).invalid
        assertEquals(PortalNaming.Reason.HIDDEN_CHARACTERS, hidden.reason)
    }

    @Test
    fun aChosenColorMustBeFreeAndValid() {
        val existing = portals(2)
        assertEquals(5, (check(existing, 50, "Sky", 5) as Decision.Admit).colorIndex)
        val taken = (check(existing, 50, "Sky", 1) as Decision.Refuse).refusal as Refusal.ColorTaken
        assertEquals(2L, taken.portal.identity.id)
        assertEquals(Decision.Refuse(Refusal.BadColor(10)), check(existing, 50, "Sky", 10))
        assertEquals(Decision.Refuse(Refusal.BadColor(-1)), check(existing, 50, "Sky", -1))
    }

    @Test
    fun aColorFreedByARemovalCanBeUsedAgain() {
        // Portal 2 (color 1) was removed: its color and its name are free at once.
        val existing = listOf(portals(3)[0], portals(3)[2])
        val d = check(existing, 50, "Portal 2", 1) as Decision.Admit
        assertEquals(1, d.colorIndex)
    }

    @Test
    fun theColorThatLooksLikeTheNewestPortalIsSkipped() {
        // The newest portal has coral (7). Sand (3) is free and looks like coral, sky (4) is free too.
        val existing = listOf(
            Portals.known(1, "A", 0, "https://a.example.org"),
            Portals.known(2, "B", 1, "https://b.example.org"),
            Portals.known(3, "C", 2, "https://c.example.org"),
            Portals.known(4, "D", 7, "https://d.example.org"),
        )
        assertEquals(4, (check(existing, 50, "E") as Decision.Admit).colorIndex)
    }

    @Test
    fun aTextThatIsNotAFingerprintIsAProgrammingError() {
        try {
            PortalAdmission.check(emptyList(), "nope", "X", null)
            org.junit.Assert.fail()
        } catch (e: IllegalArgumentException) {
        }
    }
}
