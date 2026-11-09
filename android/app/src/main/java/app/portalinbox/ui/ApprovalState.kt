package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.Item
import app.portalinbox.data.OutboxAnswer
import app.portalinbox.data.Validity
import app.portalinbox.proto.Intent
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.RequestKind

/**
 * What the approval screen lets the owner do. Pure Kotlin: the Activity gives the facts and draws the answer.
 *
 * Rules of Approve (CONF 5.3, decided with Alice):
 * - Approve is shown only for an open approval whose effect the phone checked.
 * - Approve is locked until the command was read to the end ([ReadGate]).
 * - Past `stale_after`, Approve also needs the box "I know this request is N days old".
 * - The phone needs a screen lock or a biometric: the local prompt is the last gate. Without it Approve stays locked.
 * - While an answer is on its way, both buttons are locked.
 * - Decline needs no gate. It never runs the command, so it is the safe answer: it works from the notification too.
 */
object ApprovalState {
    /**
     * The age at which a request counts as old. The phone gets no value from the server: this is the default
     * of the server key `inbox.approvals.stale_after` (24 h, CONF 6.7).
     */
    const val STALE_AFTER_MS = 24L * 3_600_000

    /** Longest text of a decline (CONF 4.10). The field of the screen stops at this length: nothing is cut later. */
    const val MAX_DECLINE_CHARS = 2000

    enum class Mode {
        /** The owner can answer. */
        OPEN,

        /** An answer is on its way. */
        SENDING,

        /** The phone does not trust the request (`Invalid request`). Only Decline works. */
        INVALID,

        /** The item is final. The screen is read only. */
        CLOSED,
    }

    /** Why Approve is locked. The first reason of this order wins. */
    enum class Lock { READ_COMMAND, CONFIRM_STALE, NO_SCREEN_LOCK, SENDING, PORTAL_BLOCKED, AGENT_LOCKED }

    /**
     * What the phone can do with an answer for this portal now (proposal 3.11). [ONLINE]: it goes at once. [OFFLINE]: the outbox
     * holds it until the portal is back, and the server decides then. [BLOCKED]: the key of the portal changed or the portal
     * revoked this device. Nothing can be sent, so Approve and Decline are off.
     */
    enum class Link { ONLINE, OFFLINE, BLOCKED }

    /** Maps the state of the connection to [Link]. A connection that is starting counts as online: it opens within seconds. */
    fun linkOf(state: app.portalinbox.service.LinkState): Link = when (state) {
        app.portalinbox.service.LinkState.CONNECTED, app.portalinbox.service.LinkState.CONNECTING -> Link.ONLINE
        app.portalinbox.service.LinkState.DISCONNECTED -> Link.OFFLINE
        app.portalinbox.service.LinkState.REENROLL, app.portalinbox.service.LinkState.KEY_CHANGED -> Link.BLOCKED
    }

    /** "Offline. Your answer is sent when Production is back." Null when the portal is online. */
    fun offlineLine(link: Link, nickname: String): String? = when (link) {
        Link.ONLINE -> null
        Link.OFFLINE -> "Offline. Your answer is sent when ${VisibleText.escapeLine(nickname)} is back."
        Link.BLOCKED -> "Not connected. Nothing is sent to ${VisibleText.escapeLine(nickname)}."
    }

    data class Input(
        val item: Item,
        val nowMs: Long,
        val gate: ReadGate.State,
        val staleConfirmed: Boolean,
        val answer: AnswerProgress,
        /** The phone can ask for a biometric or a device credential. */
        val canAuthenticate: Boolean,
        /** Lot A3 gives the discussion screen. Without it the button "Ask the agent" does not show. */
        val askAvailable: Boolean,
        /** The state of the link to the portal of the item. */
        val link: Link = Link.ONLINE,
        /** The nickname of the portal, for the offline line. */
        val portal: String = "this portal",
        /** The agent of the item is new and the portal locks the new agents (lot G4): Approve stays off until the owner allows the agent. */
        val agentLocked: Boolean = false,
    )

    data class Screen(
        val mode: Mode,
        val ageMs: Long,
        val stale: Boolean,
        val approveShown: Boolean,
        /** Null when Approve is active. */
        val approveLock: Lock?,
        val declineEnabled: Boolean,
        val declineWithComment: Boolean,
        val askShown: Boolean,
        val staleBoxShown: Boolean,
        val staleBoxText: String?,
        /** The line above the bottom bar. */
        val hint: String?,
        /** The red or amber line at the top for an invalid request or a closed item. */
        val notice: String?,
        /** The line about the link: offline or blocked. Null when the portal is online or the item is closed. */
        val linkLine: String? = null,
        /** The button says "Queue approval": the portal is offline and the answer waits on the phone. */
        val queued: Boolean = false,
    ) {
        val approveEnabled: Boolean get() = approveShown && approveLock == null
    }

    fun ageOf(item: Item, nowMs: Long): Long = (nowMs - item.request.createdAtMs).coerceAtLeast(0)

    fun isStale(ageMs: Long): Boolean = ageMs > STALE_AFTER_MS

    fun of(i: Input): Screen {
        val item = i.item
        val age = ageOf(item, i.nowMs)
        val stale = isStale(age) && item.isOpen
        val mode = when {
            item.kind != RequestKind.APPROVAL || !item.isOpen -> Mode.CLOSED
            i.answer.pending -> Mode.SENDING
            item.validity != Validity.VALID -> Mode.INVALID
            else -> Mode.OPEN
        }
        val approveShown = mode == Mode.OPEN || mode == Mode.SENDING
        val blocked = i.link == Link.BLOCKED && mode != Mode.CLOSED
        val lock = when {
            mode == Mode.SENDING -> Lock.SENDING
            blocked -> Lock.PORTAL_BLOCKED
            i.agentLocked -> Lock.AGENT_LOCKED
            !i.gate.readToEnd -> Lock.READ_COMMAND
            stale && !i.staleConfirmed -> Lock.CONFIRM_STALE
            !i.canAuthenticate -> Lock.NO_SCREEN_LOCK
            else -> null
        }
        val declineEnabled = (mode == Mode.OPEN || mode == Mode.INVALID) && !blocked
        val staleBox = stale && approveShown
        return Screen(
            mode = mode,
            ageMs = age,
            stale = stale,
            approveShown = approveShown,
            approveLock = if (approveShown) lock else null,
            declineEnabled = declineEnabled,
            declineWithComment = declineEnabled && item.request.allowCustom,
            askShown = mode == Mode.OPEN && item.request.allowAskBack && i.askAvailable,
            staleBoxShown = staleBox,
            staleBoxText = if (staleBox) "I know this request is ${Ages.long(age)} old." else null,
            hint = hintOf(mode, if (approveShown) lock else null, i.answer),
            notice = noticeOf(item, mode),
            linkLine = if (mode == Mode.CLOSED) null else offlineLine(i.link, i.portal),
            queued = i.link == Link.OFFLINE && mode == Mode.OPEN,
        )
    }

    private fun hintOf(mode: Mode, lock: Lock?, answer: AnswerProgress): String? = when (mode) {
        Mode.CLOSED -> null
        Mode.INVALID -> "The phone does not approve this request. You can decline it."
        Mode.SENDING -> OutboxAnswer.pendingLine(answer)
        Mode.OPEN -> when (lock) {
            Lock.READ_COMMAND -> "Scroll the command to its end to unlock Approve."
            Lock.CONFIRM_STALE -> "Tick the box to unlock Approve."
            Lock.NO_SCREEN_LOCK -> "Set a screen lock or a fingerprint on this phone to approve."
            Lock.PORTAL_BLOCKED -> "Nothing can be sent to this portal now."
            Lock.AGENT_LOCKED -> AgentTexts.LOCK_HINT
            Lock.SENDING -> null
            null -> "Approve asks for your fingerprint or your device PIN."
        }
    }

    private fun noticeOf(item: Item, mode: Mode): String? = when {
        mode == Mode.INVALID -> when (item.validity) {
            Validity.EFFECT_MISMATCH -> "Invalid request. The command does not match its hash. The phone does not approve it."
            Validity.INVALID_OP -> "Invalid request. The command breaks the rules of the protocol. The phone does not approve it."
            Validity.INVALID_VIEW -> "Invalid request. The screen layout of the command is wrong. The phone does not approve it."
            Validity.VALID -> null
        }
        item.kind == RequestKind.APPROVAL && !item.isOpen -> ItemTexts.closedNotice(item)
        else -> null
    }

    // ------------------------------------------------------------ decline with a comment

    /** The text and the intent of a decline, or the reason that the text is not valid. */
    sealed interface Decline {
        /** A decline without text. */
        data object Plain : Decline

        data class WithText(val text: String, val intent: Intent) : Decline

        data class Refused(val reason: String) : Decline
    }

    /**
     * Makes the decline of the form. A text of only spaces is no text. The text is not changed: the owner
     * wrote it. "Propose a change" gives the intent `change_request`, else `comment`.
     */
    fun decline(text: String, proposeChange: Boolean, allowCustom: Boolean): Decline {
        if (text.isBlank()) return Decline.Plain
        if (!allowCustom) return Decline.Refused("The agent does not take a text with a decline.")
        if (text.codePointCount(0, text.length) > MAX_DECLINE_CHARS) {
            return Decline.Refused("The text is too long. Use $MAX_DECLINE_CHARS characters at most.")
        }
        if (text.contains('\u0000')) return Decline.Refused("The text has a character that is not allowed.")
        return Decline.WithText(text, if (proposeChange) Intent.CHANGE_REQUEST else Intent.COMMENT)
    }

    /** True when an open item can still get an answer from the screen. */
    fun answerable(item: Item): Boolean = item.state == ItemState.PENDING || item.state == ItemState.REPLIED
}
