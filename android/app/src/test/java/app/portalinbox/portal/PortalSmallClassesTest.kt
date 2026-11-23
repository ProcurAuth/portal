package app.portalinbox.portal

import app.portalinbox.data.ItemEngine
import app.portalinbox.service.LinkState
import app.portalinbox.service.LinkStatus
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.Portals
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The small pure classes of the portal list and of the portal screen. */
class PortalSmallClassesTest {
    // ------------------------------------------------------------ PortalRow

    private val now = 1_800_000_000_000L

    @Test
    fun theRowHoldsOnlyLocalIdentityAndTheKeyPrefix() {
        val server = Portals.server(id = 3, nickname = "Production", color = 0, keyNo = 5, address = "https://portal.example.org:8443")
            .copy(lastSyncMs = now - 60_000, addedAtMs = now - 3_600_000)
        val row = PortalRow.of(server, LinkStatus(LinkState.CONNECTED, now - 5_000, null), openCount = 2, nowMs = now)
        assertEquals("Production", row.identity.nickname)
        assertEquals("PR", row.identity.monogram)
        assertEquals("portal.example.org:8443", row.host)
        assertEquals("alice", row.login)
        assertEquals(Fingerprint.short(Portals.fp(5), 3), row.keyPrefix)
        assertEquals(3, row.keyPrefix.split(' ').size)
        assertTrue(row.isNew)
        assertEquals(LinkState.CONNECTED, row.state)
        assertEquals(now - 60_000, row.lastSyncMs)
        assertEquals(2, row.openCount)
        assertFalse(row.needsUser)
    }

    @Test
    fun isNewEndsAfter24Hours() {
        val old = Portals.server().copy(addedAtMs = now - 25L * 3_600_000)
        assertFalse(PortalRow.of(old, null, 0, now).isNew)
    }

    @Test
    fun withoutAStatusTheStoredReasonDecidesTheState() {
        assertEquals(LinkState.CONNECTING, PortalRow.of(Portals.server(), null, 0, now).state)
        val changed = Portals.server().copy(reenrollReason = "key_changed: x")
        assertEquals(LinkState.KEY_CHANGED, PortalRow.of(changed, null, 0, now).state)
        assertTrue(PortalRow.of(changed, null, 0, now).needsUser)
        val revoked = Portals.server().copy(reenrollReason = "Device revoked")
        assertEquals(LinkState.REENROLL, PortalRow.of(revoked, null, 0, now).state)
    }

    @Test
    fun theReportedNameShowsOnlyWhenItDiffers() {
        assertNull(PortalRow.reportedNameIfDifferent("Production", "production"))
        assertNull(PortalRow.reportedNameIfDifferent("Production", "  "))
        assertEquals("Prod", PortalRow.reportedNameIfDifferent("Production", "Prod"))
        // The portal chooses this text: a hidden character is made visible.
        val shown = PortalRow.reportedNameIfDifferent("Production", "Prod‮uction")!!
        assertFalse(shown.contains('‮'))
    }

    @Test
    fun hostOfDecodesPunycodeAndKeepsThePort() {
        assertEquals("10.8.0.5:8443", PortalRow.hostOf("http://10.8.0.5:8443"))
        assertEquals("portál.example.org", PortalRow.hostOf("https://xn--portl-0qa.example.org"))
        assertEquals("portal.example.org", PortalRow.hostOf("https://portal.example.org/inbox"))
    }

    // ------------------------------------------------------------ KeyChangedModel

    @Test
    fun keyChangedModelShowsTheOldFingerprintOnly() {
        assertNull(KeyChangedModel.of(Portals.server()))
        val s = Portals.server(keyNo = 4).copy(reenrollReason = "key_changed: The Portal did not prove the key", prevKeyFp = Fingerprint.compact(Portals.fp(2)), keyChangedMs = 5L)
        val m = KeyChangedModel.of(s)!!
        assertEquals(Portals.fp(4), m.oldFingerprint)
        assertEquals(Portals.fp(2), m.previousFingerprint)
        assertEquals(5L, m.previousChangedMs)
        assertTrue(m.keyProblem)
        assertEquals("The Portal did not prove the key", m.detail)
    }

    @Test
    fun aRevokedDeviceIsNotAKeyProblem() {
        val m = KeyChangedModel.of(Portals.server().copy(reenrollReason = "Device revoked by the administrator: delete this server and add it again"))!!
        assertFalse(m.keyProblem)
        assertNull(m.previousFingerprint)
    }

    // ------------------------------------------------------------ AddressChange

    private val prod = Portals.known(1, "Production", 0, "https://portal.example.org")
    private val stg = Portals.known(2, "Staging", 1, "http://10.0.0.5:8080")

    @Test
    fun addressChangeAcceptsANewAddress() {
        val r = AddressChange.check("10.8.0.9:8443", prod, listOf(prod, stg))
        assertEquals(AddressChange.Result.Ok("http://10.8.0.9:8443"), r)
        assertEquals(AddressChange.Result.Ok("https://portal2.example.org"), AddressChange.check(" https://portal2.example.org/ ", prod, listOf(prod, stg)))
    }

    @Test
    fun addressChangeRefusesBadSameAndUsedAddresses() {
        assertEquals(AddressChange.Result.Invalid, AddressChange.check("not an address", prod, listOf(prod)))
        assertEquals(AddressChange.Result.Invalid, AddressChange.check("", prod, listOf(prod)))
        assertEquals(AddressChange.Result.Same, AddressChange.check("https://portal.example.org/", prod, listOf(prod)))
        val used = AddressChange.check("http://10.0.0.5:8080", prod, listOf(prod, stg)) as AddressChange.Result.UsedBy
        assertEquals(2L, used.portal.identity.id)
    }

    // ------------------------------------------------------------ PortalReorder

    @Test
    fun moveKeepsEachIdOnce() {
        val order = listOf(1L, 2, 3, 4)
        assertEquals(listOf(1L, 3, 4, 2), PortalReorder.move(order, 2, 3))
        assertEquals(listOf(4L, 1, 2, 3), PortalReorder.move(order, 4, 0))
        assertEquals(listOf(1L, 2, 3, 4), PortalReorder.move(order, 3, 2))
        assertEquals(order, PortalReorder.move(order, 99, 0))
        assertEquals(listOf(2L, 3, 4, 1), PortalReorder.move(order, 1, 99))
        assertEquals(listOf(1L, 2, 3, 4), PortalReorder.move(order, 1, -5))
    }

    @Test
    fun upAndDownStopAtTheEnds() {
        val order = listOf(1L, 2, 3)
        assertEquals(listOf(2L, 1, 3), PortalReorder.up(order, 2))
        assertEquals(order, PortalReorder.up(order, 1))
        assertEquals(listOf(1L, 3, 2), PortalReorder.down(order, 2))
        assertEquals(order, PortalReorder.down(order, 3))
    }

    @Test
    fun targetIndexIsTheRowWhoseMiddleIsClosest() {
        val tops = listOf(0, 100, 200)
        val heights = listOf(100, 100, 100)
        assertEquals(0, PortalReorder.targetIndex(tops, heights, 10f))
        assertEquals(1, PortalReorder.targetIndex(tops, heights, 160f))
        assertEquals(2, PortalReorder.targetIndex(tops, heights, 900f))
        assertEquals(0, PortalReorder.targetIndex(emptyList(), emptyList(), 5f))
    }

    // ------------------------------------------------------------ RemovalSummary

    @Test
    fun theSummaryCountsOpenUnsentAndHistory() {
        var n = 1_800_000_000_000L
        val repo = MemoryItemRepository()
        val engine = ItemEngine(repo, ReferenceEffectHasher(), { n++ }, { "m-" + n })
        // Portal 1: two open items, one finished.
        engine.onRequest(1, Make.approval(Make.hex(0xa), Make.hex(0x101)))
        engine.onRequest(1, Make.approval(Make.hex(0xb), Make.hex(0x102)))
        engine.onRequest(1, Make.approval(Make.hex(0xc), Make.hex(0x103)))
        engine.onSettled(1, Make.settled(Make.hex(0xc), Make.hex(0x201)))
        // An answer that the phone did not send.
        engine.decline(1, Make.hex(0xa))
        // Portal 2 is not counted.
        engine.onRequest(2, Make.approval(Make.hex(0xd), Make.hex(0x104)))

        val s = RemovalSummary.of(repo, 1, alertCount = 3)
        assertEquals(2, s.openItems)
        assertEquals(1, s.unsentAnswers)
        assertEquals(1, s.historyItems)
        assertEquals(3, s.alerts)
        assertFalse(s.empty)
        assertTrue(RemovalSummary.of(repo, 9, 0).empty)
    }
}

class FingerprintTextTest {
    private val fp = "0d47 9be3 c1a8 6f52 b3e7"

    @Test
    fun threeGroupsOnTheFirstLine() {
        assertEquals("0d47 9be3 c1a8\n6f52 b3e7", FingerprintText.lines(fp))
        assertEquals("0d47 9be3 c1a8\n6f52 b3e7", FingerprintText.lines("0D479BE3C1A86F52B3E7"))
        assertEquals("0d47 9be3\nc1a8 6f52\nb3e7", FingerprintText.lines(fp, 2))
        assertEquals(fp, FingerprintText.lines(fp, 5))
    }

    @Test
    fun aTextThatIsNotAFingerprintIsKept() {
        assertEquals("nope", FingerprintText.lines("nope"))
        assertEquals("nope", FingerprintText.spoken("nope"))
    }

    @Test
    fun spokenSpellsEveryCharacter() {
        assertEquals("0 d 4 7, 9 b e 3, c 1 a 8, 6 f 5 2, b 3 e 7", FingerprintText.spoken(fp))
    }
}
