package app.portalinbox.portal

import app.portalinbox.portal.EnrollPlan.Blocker
import app.portalinbox.portal.EnrollPlan.Code
import app.portalinbox.portal.EnrollPlan.Entry
import app.portalinbox.portal.EnrollPlan.Mode
import app.portalinbox.portal.EnrollPlan.Source
import app.portalinbox.portal.EnrollPlan.TickReason
import app.portalinbox.testsupport.Portals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollPlanTest {
    private val prod = Portals.known(1, "Production", 0, "https://portal.example.org")
    private val staging = Portals.known(2, "Staging", 1, "http://10.0.0.5:8080")
    private val existing = listOf(prod, staging)
    private val typed = Source.Typed
    private val link = Source.Link("android-app://com.android.chrome")

    private fun code(address: String, keyNo: Int) = Code(address, Portals.fp(keyNo))
    private fun plan(
        c: Code, source: Source = typed, mode: Mode = Mode.Add, list: List<KnownPortal> = existing, entry: Entry = Entry(nickname = "Client A"),
    ) = EnrollPlan.plan(c, source, mode, list, entry)

    private val fresh = code("https://other.example.net", 9)

    // ------------------------------------------------------------ the table of the proposal

    @Test
    fun typedCodeWithNoFindingNeedsNoTick() {
        val p = plan(fresh)
        assertNull(p.blocker)
        assertEquals(LookAlike.Finding.None, p.finding)
        assertFalse(p.tickRequired)
        assertNull(p.tickReason)
        assertTrue(p.canAdd)
        assertFalse(p.fromLink)
    }

    @Test
    fun linkNeedsTheTickEvenWithNoFinding() {
        val p = plan(fresh, source = link)
        assertTrue(p.fromLink)
        assertTrue(p.tickRequired)
        assertEquals(TickReason.LINK, p.tickReason)
        assertFalse(p.canAdd)
        assertTrue(plan(fresh, source = link, entry = Entry("Client A", ticked = true)).canAdd)
    }

    @Test
    fun lookAlikeNeedsTheTickAndShowsTheChangedLetter() {
        val p = plan(code("https://porta1.example.org", 9))
        assertTrue(p.finding is LookAlike.Finding.SimilarAddress)
        assertTrue(p.lookAlike)
        assertTrue(p.tickRequired)
        assertEquals(TickReason.LOOK_ALIKE, p.tickReason)
        assertEquals("porta1.example.org", p.shownAddress)
        assertEquals("portal.example.org", p.otherAddress)
        assertEquals(listOf(5..5), p.changedRanges)
        assertFalse(p.canAdd)
        assertTrue(plan(code("https://porta1.example.org", 9), entry = Entry("Client A", ticked = true)).canAdd)
    }

    @Test
    fun lookAlikeWinsOverLinkForTheReason() {
        val p = plan(code("https://porta1.example.org", 9), source = link)
        assertEquals(TickReason.LOOK_ALIKE, p.tickReason)
    }

    @Test
    fun sameAddressWithAnotherKeyIsADangerAndNeedsTheTick() {
        val p = plan(code("https://portal.example.org", 9))
        val f = p.finding as LookAlike.Finding.SameAddressOtherKey
        assertEquals(1L, f.portal.identity.id)
        assertTrue(p.lookAlike)
        assertTrue(p.tickRequired)
        assertTrue(p.changedRanges.isEmpty())
        assertEquals("portal.example.org", p.otherAddress)
    }

    @Test
    fun aLookAlikeNameNeedsTheTickToo() {
        val p = plan(fresh, entry = Entry("Pr0duction"))
        assertTrue(p.finding is LookAlike.Finding.SimilarName)
        assertTrue(p.tickRequired)
        // The name is taken, as well: the normalized names are equal.
        assertTrue((p.nickname as PortalNaming.Check.Invalid).reason == PortalNaming.Reason.TAKEN)
        assertFalse(p.canAdd)
        // A near name that is free: valid with the tick.
        val q = plan(fresh, entry = Entry("Production 2", ticked = true))
        assertTrue(q.finding is LookAlike.Finding.SimilarName)
        assertTrue(q.nickname is PortalNaming.Check.Valid)
        assertTrue(q.canAdd)
    }

    @Test
    fun sameKeyAddsNothingAndPointsToThePortal() {
        val p = plan(code("https://elsewhere.example.net", 1))
        val b = p.blocker as Blocker.AlreadyAdded
        assertEquals(1L, b.portal.identity.id)
        assertFalse(p.canAdd)
        // Even with the tick and a good name.
        assertFalse(plan(code("https://elsewhere.example.net", 1), source = link, entry = Entry("Client A", ticked = true)).canAdd)
    }

    @Test
    fun theEleventhPortalIsRefused() {
        val ten = (1..10).map { Portals.known(it.toLong(), "Name$it" + "x".repeat(it), it - 1, "https://h$it.example.net") }
        val p = plan(code("https://new.example.org", 99), list = ten)
        assertEquals(Blocker.CapReached, p.blocker)
        assertFalse(p.canAdd)
        assertNull(p.colorIndex)
        // Nine portals: the add is possible.
        assertTrue(plan(code("https://new.example.org", 99), list = ten.take(9)).canAdd)
    }

    @Test
    fun theNicknameIsRequiredAndNotProposed() {
        val p = plan(fresh, entry = Entry())
        assertEquals(PortalNaming.Reason.EMPTY, (p.nickname as PortalNaming.Check.Invalid).reason)
        assertFalse(p.canAdd)
        assertFalse(plan(fresh, entry = Entry("   ")).canAdd)
        assertFalse(plan(fresh, entry = Entry("A‮B")).canAdd) // bidi
        assertFalse(plan(fresh, entry = Entry("x".repeat(17))).canAdd)
        assertTrue(plan(fresh, entry = Entry("x".repeat(16))).canAdd)
    }

    @Test
    fun aTakenNameIsRefused() {
        val p = plan(fresh, entry = Entry("production"))
        assertEquals(PortalNaming.Reason.TAKEN, (p.nickname as PortalNaming.Check.Invalid).reason)
        assertFalse(p.canAdd)
    }

    @Test
    fun badCodeBlocks() {
        val p = EnrollPlan.plan(Code("not an address", "zz"), typed, Mode.Add, existing, Entry("Client A"))
        assertEquals(Blocker.BadCode, p.blocker)
        assertEquals("", p.fingerprint)
        assertFalse(p.canAdd)
    }

    // ------------------------------------------------------------ colors

    @Test
    fun theColorDefaultsToTheFirstFreeOneAndUsedColorsAreMarked() {
        val p = plan(fresh)
        // Portals use 0 (violet) and 1 (teal): the next is pink (2).
        assertEquals(2, p.colorIndex)
        assertEquals(10, p.colors.size)
        assertEquals("Production", p.colors[0].usedBy!!.nickname)
        assertEquals("Staging", p.colors[1].usedBy!!.nickname)
        assertTrue(p.colors[2].free)
    }

    @Test
    fun aChosenFreeColorApplies() {
        assertEquals(4, plan(fresh, entry = Entry("Client A", colorIndex = 4)).colorIndex)
    }

    @Test
    fun aUsedOrUnknownColorDoesNotApply() {
        val used = plan(fresh, entry = Entry("Client A", colorIndex = 0))
        assertNull(used.colorIndex)
        assertFalse(used.canAdd)
        assertNull(plan(fresh, entry = Entry("Client A", colorIndex = 10)).colorIndex)
        assertNull(plan(fresh, entry = Entry("Client A", colorIndex = -1)).colorIndex)
    }

    @Test
    fun aColorThatLooksLikeTheLastPortalIsSkippedByDefault() {
        // Colors 0, 1, 2, 4, 5, 6 and then 3 (sand) were given, so the last portal has sand. Free: 7 (coral), 8, 9. Coral looks like sand.
        val order = listOf(0, 1, 2, 4, 5, 6, 3)
        val list = order.mapIndexed { i, c -> Portals.known(i + 1L, "Name" + "x".repeat(i + 1), c, "https://h$i.example.net") }
        assertEquals(8, plan(fresh, list = list).colorIndex)
    }

    // ------------------------------------------------------------ link rules and re-enroll mode

    @Test
    fun aLinkNeverGivesReenrollMode() {
        val p = plan(code("https://portal.example.org", 9), source = link, mode = Mode.Reenroll(prod), entry = Entry(ticked = true))
        assertEquals(Blocker.LinkCannotReenroll, p.blocker)
        assertFalse(p.canAdd)
    }

    @Test
    fun reenrollKeepsTheNicknameAndTheColor() {
        val p = plan(code("https://portal.example.org", 7), mode = Mode.Reenroll(prod), entry = Entry(nickname = "Hijack", colorIndex = 5))
        assertEquals("Production", p.fixedNickname)
        assertNull(p.nickname)
        assertEquals(0, p.colorIndex)
        assertNull(p.blocker)
        // The portal itself is not a look-alike of itself.
        assertEquals(LookAlike.Finding.None, p.finding)
        assertFalse(p.lookAlike)
    }

    @Test
    fun reenrollNeedsTheTickAndShowsBothFingerprints() {
        val c = code("https://portal.example.org", 7)
        val p = plan(c, mode = Mode.Reenroll(prod))
        assertEquals(TickReason.REENROLL, p.tickReason)
        assertTrue(p.tickRequired)
        assertFalse(p.canAdd)
        assertEquals(Portals.fp(1), p.previousFingerprint)
        assertEquals(Portals.fp(7), p.fingerprint)
        assertTrue(p.keyChanged)
        assertTrue(plan(c, mode = Mode.Reenroll(prod), entry = Entry(ticked = true)).canAdd)
    }

    @Test
    fun reenrollWithTheSameKeyIsNotAKeyChange() {
        val p = plan(code("https://new-address.example.org", 1), mode = Mode.Reenroll(prod), entry = Entry(ticked = true))
        assertFalse(p.keyChanged)
        assertNull(p.blocker)
        assertTrue(p.canAdd)
    }

    @Test
    fun reenrollWithTheKeyOfAnotherPortalIsRefused() {
        val p = plan(code("https://portal.example.org", 2), mode = Mode.Reenroll(prod), entry = Entry(ticked = true))
        assertEquals(2L, (p.blocker as Blocker.KeyOfOtherPortal).portal.identity.id)
        assertFalse(p.canAdd)
    }

    @Test
    fun reenrollIgnoresTheCapAndTheNameOfTheOthers() {
        val ten = (1..10).map { Portals.known(it.toLong(), "Name$it" + "x".repeat(it), it - 1, "https://h$it.example.net") }
        val p = plan(code("https://h1.example.net", 77), mode = Mode.Reenroll(ten[0]), list = ten, entry = Entry(ticked = true))
        assertNull(p.blocker)
        assertTrue(p.canAdd)
    }

    @Test
    fun theColorsOfReenrollShowTheOthersAsUsed() {
        val p = plan(code("https://portal.example.org", 7), mode = Mode.Reenroll(prod))
        assertTrue(p.colors[0].free) // its own color is not "used by another"
        assertFalse(p.colors[1].free)
    }

    // ------------------------------------------------------------ the link hint and the address

    @Test
    fun theReferrerIsAHintAndMayBeMissing() {
        assertEquals("com.android.chrome", plan(fresh, source = link).referrerText)
        assertNull(plan(fresh, source = Source.Link(null)).referrerText)
        assertNull(plan(fresh, source = Source.Link("weird")).referrerText)
        assertNull(plan(fresh).referrerText)
    }

    @Test
    fun punycodeIsShownDecodedAndMarked() {
        val p = plan(code("https://xn--portl-0qa.example.org", 9))
        assertTrue(p.international)
        assertTrue(p.shownAddress.startsWith("portál."))
        assertFalse(plan(fresh).international)
    }

    @Test
    fun theAddressKeepsTheExplicitPortAndPath() {
        assertEquals("10.8.0.5:8443/inbox", EnrollPlan.shownAddress("http://10.8.0.5:8443/inbox"))
        assertEquals("portal.example.org", EnrollPlan.shownAddress("https://portal.example.org"))
        assertNotNull(plan(code("https://portal.example.org:8443/x", 9)).shownAddress)
    }

    @Test
    fun planIsPureAndDoesNotChangeItsInput() {
        val before = existing.toList()
        val a = plan(fresh)
        val b = plan(fresh)
        assertEquals(a, b)
        assertEquals(before, existing)
    }
}
