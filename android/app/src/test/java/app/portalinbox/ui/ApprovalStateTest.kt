package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.proto.Action
import app.portalinbox.proto.Intent
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ApprovalStateTest {
    private val server = 1L
    private val created = 1_788_000_000_000L
    private var now = created + 3 * 60_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val rid = Make.hex(0xb)
    private val read = ReadGate.State("k", true, 100)
    private val unread = ReadGate.start("k")

    private fun item(allowCustom: Boolean = true, allowAsk: Boolean = true, createdAtMs: Long = created, edit: (org.json.JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), createdAtMs = createdAtMs, allowCustom = allowCustom, allowAskBack = allowAsk, edit = edit))
        return repo.item(server, rid)!!
    }

    private fun screen(
        i: Item,
        gate: ReadGate.State = read,
        staleConfirmed: Boolean = false,
        answer: AnswerProgress = AnswerProgress.None,
        auth: Boolean = true,
        ask: Boolean = true,
    ) = ApprovalState.of(ApprovalState.Input(i, now, gate, staleConfirmed, answer, auth, ask))

    @Test
    fun approveIsLockedUntilTheCommandIsReadToTheEnd() {
        val s = screen(item(), gate = unread)
        assertTrue(s.approveShown)
        assertFalse(s.approveEnabled)
        assertEquals(ApprovalState.Lock.READ_COMMAND, s.approveLock)
        assertEquals("Scroll the command to its end to unlock Approve.", s.hint)
        assertTrue(s.declineEnabled)
    }

    @Test
    fun approveIsActiveAfterTheReadAndTheHintTellsAboutTheFingerprint() {
        val s = screen(item())
        assertTrue(s.approveEnabled)
        assertNull(s.approveLock)
        assertEquals("Approve asks for your fingerprint or your device PIN.", s.hint)
        assertEquals(ApprovalState.Mode.OPEN, s.mode)
    }

    @Test
    fun anOldRequestNeedsTheBoxBeforeApprove() {
        now = created + 6 * 86_400_000L
        val i = item()
        val locked = screen(i)
        assertTrue(locked.stale)
        assertTrue(locked.staleBoxShown)
        assertEquals("I know this request is 6 days old.", locked.staleBoxText)
        assertEquals(ApprovalState.Lock.CONFIRM_STALE, locked.approveLock)
        assertFalse(locked.approveEnabled)
        assertTrue(screen(i, staleConfirmed = true).approveEnabled)
    }

    @Test
    fun theReadGateComesBeforeTheStaleBox() {
        now = created + 6 * 86_400_000L
        assertEquals(ApprovalState.Lock.READ_COMMAND, screen(item(), gate = unread).approveLock)
    }

    @Test
    fun theStaleLimitIsStrictlyMoreThan24Hours() {
        now = created + ApprovalState.STALE_AFTER_MS
        val i = item()
        assertFalse(screen(i).stale)
        assertFalse(screen(i).staleBoxShown)
        now += 1
        assertTrue(screen(i).stale)
    }

    @Test
    fun aPhoneWithoutScreenLockCannotApprove() {
        val s = screen(item(), auth = false)
        assertEquals(ApprovalState.Lock.NO_SCREEN_LOCK, s.approveLock)
        assertFalse(s.approveEnabled)
        assertTrue(s.declineEnabled)
        assertEquals("Set a screen lock or a fingerprint on this phone to approve.", s.hint)
    }

    @Test
    fun whileAnAnswerIsOnItsWayBothButtonsAreLocked() {
        for (p in listOf(AnswerProgress.Queued(Action.ACCEPT), AnswerProgress.Sent(Action.DECLINE))) {
            val s = screen(item(), answer = p)
            assertEquals(ApprovalState.Mode.SENDING, s.mode)
            assertFalse(s.approveEnabled)
            assertFalse(s.declineEnabled)
            assertFalse(s.declineWithComment)
            assertEquals(ApprovalState.Lock.SENDING, s.approveLock)
        }
    }

    @Test
    fun anInvalidRequestOnlyAllowsDecline() {
        val bad = engine.run {
            onRequest(server, Make.approval(rid, Make.hex(1), effect = "sha256:" + "0".repeat(64)))
            repo.item(server, rid)!!
        }
        val s = screen(bad)
        assertEquals(ApprovalState.Mode.INVALID, s.mode)
        assertFalse(s.approveShown)
        assertFalse(s.approveEnabled)
        assertTrue(s.declineEnabled)
        assertTrue(s.notice!!.startsWith("Invalid request."))
    }

    @Test
    fun aClosedItemIsReadOnlyAndSaysHowItEnded() {
        item()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        val s = screen(repo.item(server, rid)!!)
        assertEquals(ApprovalState.Mode.CLOSED, s.mode)
        assertFalse(s.approveShown)
        assertFalse(s.declineEnabled)
        assertFalse(s.askShown)
        assertEquals("The agent no longer answers. You cannot answer this request.", s.notice)
        assertNull(s.hint)
    }

    @Test
    fun theFlagsDecideTheExtraButtons() {
        val open = item(allowCustom = true, allowAsk = true)
        val s = screen(open)
        assertTrue(s.declineWithComment)
        assertTrue(s.askShown)
        assertFalse(screen(open, ask = false).askShown)
    }

    @Test
    fun withoutTheFlagsThereIsNoCommentFieldAndNoQuestionButton() {
        val s = screen(item(allowCustom = false, allowAsk = false))
        assertFalse(s.declineWithComment)
        assertFalse(s.askShown)
        assertTrue(s.declineEnabled)
    }

    @Test
    fun theAgeComesFromTheCreationOfTheRequest() {
        val i = item()
        assertEquals(3 * 60_000L, ApprovalState.ageOf(i, now))
        assertEquals(0L, ApprovalState.ageOf(i, created - 10_000))
    }

    // ------------------------------------------------------------ the decline form

    @Test
    fun aDeclineWithNoTextIsPlain() {
        assertEquals(ApprovalState.Decline.Plain, ApprovalState.decline("", false, true))
        assertEquals(ApprovalState.Decline.Plain, ApprovalState.decline("  \n ", true, true))
    }

    @Test
    fun aTextGivesACommentOrAChangeRequest() {
        assertEquals(ApprovalState.Decline.WithText("use staging_b", Intent.COMMENT), ApprovalState.decline("use staging_b", false, true))
        assertEquals(ApprovalState.Decline.WithText("use staging_b", Intent.CHANGE_REQUEST), ApprovalState.decline("use staging_b", true, true))
    }

    @Test
    fun theTextIsNotChanged() {
        val d = ApprovalState.decline("  keep my spaces \n", false, true) as ApprovalState.Decline.WithText
        assertEquals("  keep my spaces \n", d.text)
    }

    @Test
    fun aTextIsRefusedWhenTheAgentDoesNotTakeIt() {
        assertTrue(ApprovalState.decline("why", false, false) is ApprovalState.Decline.Refused)
    }

    @Test
    fun aTooLongTextIsRefusedNeverCut() {
        val ok = "x".repeat(ApprovalState.MAX_DECLINE_CHARS)
        assertTrue(ApprovalState.decline(ok, false, true) is ApprovalState.Decline.WithText)
        assertTrue(ApprovalState.decline(ok + "x", false, true) is ApprovalState.Decline.Refused)
    }

    @Test
    fun theDeclineOfTheFormGoesThroughTheEngine() {
        val i = item()
        val d = ApprovalState.decline("use staging_b", true, i.request.allowCustom) as ApprovalState.Decline.WithText
        val r = engine.decline(server, rid, d.text, d.intent)
        assertTrue(r is app.portalinbox.data.Submit.Queued)
    }

    // ------------------------------------------------------------ the link to the portal (lot MP4)

    private fun linked(i: Item, link: ApprovalState.Link) =
        ApprovalState.of(ApprovalState.Input(i, now, read, false, AnswerProgress.None, true, true, link, "Homelab"))

    @Test
    fun anOnlinePortalHasNoLinkLineAndTheButtonSaysApprove() {
        val s = linked(item(), ApprovalState.Link.ONLINE)
        assertNull(s.linkLine)
        assertFalse(s.queued)
        assertTrue(s.approveEnabled)
    }

    @Test
    fun anOfflinePortalStillAllowsApproveAndDeclineAndQueuesTheAnswer() {
        val s = linked(item(), ApprovalState.Link.OFFLINE)
        assertEquals("Offline. Your answer is sent when Homelab is back.", s.linkLine)
        assertTrue(s.queued)
        assertTrue(s.approveEnabled)
        assertTrue(s.declineEnabled)
    }

    @Test
    fun aBlockedPortalTurnsApproveAndDeclineOff() {
        val s = linked(item(), ApprovalState.Link.BLOCKED)
        assertEquals("Not connected. Nothing is sent to Homelab.", s.linkLine)
        assertFalse(s.approveEnabled)
        assertEquals(ApprovalState.Lock.PORTAL_BLOCKED, s.approveLock)
        assertFalse(s.declineEnabled)
        assertFalse(s.queued)
        assertEquals("Nothing can be sent to this portal now.", s.hint)
    }

    @Test
    fun aClosedItemShowsNoLinkLine() {
        val closed = item().copy(state = app.portalinbox.proto.ItemState.ANSWERED)
        assertNull(linked(closed, ApprovalState.Link.OFFLINE).linkLine)
    }

    @Test
    fun theLinkStateMapsToWhatTheScreenCanDo() {
        use(app.portalinbox.service.LinkState.CONNECTED, ApprovalState.Link.ONLINE)
        use(app.portalinbox.service.LinkState.CONNECTING, ApprovalState.Link.ONLINE)
        use(app.portalinbox.service.LinkState.DISCONNECTED, ApprovalState.Link.OFFLINE)
        use(app.portalinbox.service.LinkState.REENROLL, ApprovalState.Link.BLOCKED)
        use(app.portalinbox.service.LinkState.KEY_CHANGED, ApprovalState.Link.BLOCKED)
    }

    private fun use(state: app.portalinbox.service.LinkState, expected: ApprovalState.Link) = assertEquals(expected, ApprovalState.linkOf(state))
}
