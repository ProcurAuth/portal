package app.portalinbox.proto

import org.json.JSONArray
import org.json.JSONObject

// Frames of protocol 5, read and written with the rules of `crates/inbox-core/src/proto.rs`.
// The golden files under `crates/inbox-core/tests/fixtures/frames` are the contract: `FramesTest`
// reads every one of them. A parser refuses an unknown frame, an unknown member, a duplicate
// member and each value that breaks a rule. It never repairs a frame.

// ---------------------------------------------------------------- vocabulary

enum class RequestKind(override val wire: String) : Wire { QUESTION("question"), APPROVAL("approval") }

enum class Urgency(override val wire: String) : Wire { LOW("low"), NORMAL("normal"), HIGH("high") }

enum class LeaseMode(override val wire: String) : Wire { LEASE("lease"), UNTIL("until") }

enum class Via(override val wire: String) : Wire { MCP("mcp"), API("api") }

enum class Interaction(override val wire: String) : Wire { CONVERSATIONAL("conversational"), ONE_SHOT("one_shot") }

enum class InputType(override val wire: String) : Wire {
    CHOICE("choice"), CONFIRM("confirm"), TEXT("text"), NONE("none"), FORM("form")
}

/** The type of a question of a form. */
enum class FormQuestionType(override val wire: String) : Wire { CHOICE("choice"), TEXT("text") }

/**
 * How a screen reads the `explanation` of an agent. A missing member means `PLAIN`. It applies to the
 * explanation only: a command, an answer and a reply stay plain text.
 */
enum class ExplanationFormat(override val wire: String) : Wire { PLAIN("plain"), MARKDOWN("markdown") }

/** The closed list of the languages of the color (`inbox_core::render::highlight::Language`). */
enum class Language(override val wire: String) : Wire {
    PLAIN("plain"), SQL("sql"), JSON("json"), YAML("yaml"), TOML("toml"), BASH("bash"), PYTHON("python"),
    RUST("rust"), JAVASCRIPT("javascript"), TYPESCRIPT("typescript"), DIFF("diff");
}

enum class Intent(override val wire: String) : Wire { COMMENT("comment"), CHANGE_REQUEST("change_request") }

enum class Author(override val wire: String) : Wire { AGENT("agent"), USER("user") }

enum class Channel(override val wire: String) : Wire { PHONE("phone"), WEB("web") }

enum class Decision(override val wire: String) : Wire { APPROVE("approve"), DENY("deny") }

/** The state of an item. `PENDING` and `REPLIED` are open. */
enum class ItemState(override val wire: String) : Wire {
    PENDING("pending"), REPLIED("replied"), ANSWERED("answered"), ABANDONED("abandoned"),
    EXPIRED("expired"), CANCELLED("cancelled"), SUPERSEDED("superseded");

    val isOpen: Boolean get() = this == PENDING || this == REPLIED
    val isFinal: Boolean get() = !isOpen

    companion object {
        fun of(wire: String): ItemState? = entries.firstOrNull { it.wire == wire }
    }
}

enum class AnswerStatus(override val wire: String) : Wire {
    ACCEPTED("accepted"), ALREADY_ANSWERED("already_answered"), CLOSED("closed"), ABANDONED("abandoned"),
    SUPERSEDED("superseded"), EXPIRED("expired"), CANCELLED("cancelled"), EFFECT_MISMATCH("effect_mismatch"),
    DEVICE_REVOKED("device_revoked"), CUSTOM_NOT_ALLOWED("custom_not_allowed"),
    ASK_BACK_NOT_ALLOWED("ask_back_not_allowed")
}

enum class Action(override val wire: String) : Wire { ACCEPT("accept"), DECLINE("decline") }

enum class DigestMode(override val wire: String) : Wire { OFF("off"), DAILY("daily"), EVERY("every") }

// ---------------------------------------------------------------- shared parts

data class Sender(val label: String, val keyName: String, val owner: String, val via: Via, val interaction: Interaction) {
    fun toJson(): JSONObject = JSONObject().put("label", label).put("key_name", keyName).put("owner", owner)
        .put("via", via.wire).put("interaction", interaction.wire)

    companion object {
        internal fun read(o: Obj): Sender {
            val s = Sender(
                Check.line(o.str("label"), 64, "from.label"),
                Check.line(o.str("key_name"), 64, "from.key_name"),
                Check.line(o.str("owner"), 64, "from.owner"),
                o.enum("via", Via.entries.toTypedArray()),
                o.enum("interaction", Interaction.entries.toTypedArray()),
            )
            o.done()
            return s
        }
    }
}

/**
 * The exact text to run or to confirm. Plain text, shown in a fixed font. [language] is a hint for the
 * color: it never changes the text.
 */
data class Command(val text: String, val label: String?, val language: Language? = null) {
    fun toJson(): JSONObject = JSONObject().put("text", text).putOpt("label", label).putOpt("language", language?.wire)

    companion object {
        internal fun read(o: Obj): Command {
            val text = o.str("text")
            if (text.isEmpty() || Check.utf8Size(text) > MAX_COMMAND_BYTES) Check.bad("command.text")
            val label = o.optStr("label")?.let { Check.line(it, MAX_COMMAND_LABEL_CHARS, "command.label") }
            val language = o.optEnum("language", Language.entries.toTypedArray())
            o.done()
            return Command(text, label, language)
        }
    }
}

data class InputOption(val id: String, val label: String, val description: String?) {
    fun toJson(): JSONObject = JSONObject().put("id", id).put("label", label).putOpt("description", description)
}

/** One question of a form. The rules of the options are those of a `choice` input. */
data class FormQuestion(
    val id: String,
    val header: String,
    val text: String,
    val type: FormQuestionType,
    val multiple: Boolean,
    val min: Int,
    val max: Int,
    val options: List<InputOption>,
    val placeholder: String?,
) {
    fun toJson(): JSONObject = JSONObject().put("id", id).put("header", header).put("text", text).put("type", type.wire)
        .put("multiple", multiple).put("min", min).put("max", max)
        .put("options", JSONArray(options.map { it.toJson() })).putOpt("placeholder", placeholder)

    companion object {
        internal fun read(o: Obj): FormQuestion {
            val id = o.str("id")
            val header = o.str("header")
            val text = o.str("text")
            val type = o.enum("type", FormQuestionType.entries.toTypedArray())
            val multiple = o.bool("multiple")
            val min = o.long("min")
            val max = o.long("max")
            if (min !in 0..255 || max !in 0..255) Check.bad("form.questions.options")
            val options = readOptions(o.arr("options"), "form.questions.options")
            val placeholder = o.optStr("placeholder")
            o.done()
            val q = FormQuestion(id, header, text, type, multiple, min.toInt(), max.toInt(), options, placeholder)
            q.validate()
            return q
        }
    }

    internal fun validate() {
        if (!Check.optionId(id)) Check.bad("form.questions.id")
        Check.line(header, MAX_FORM_HEADER_CHARS, "form.questions.header")
        Check.text(text, MAX_FORM_TEXT_CHARS, "form.questions.text")
        when (type) {
            FormQuestionType.CHOICE -> {
                Check.choice(multiple, min, max, options, "form.questions.options")
                if (placeholder != null) Check.bad("form.questions.placeholder")
            }
            FormQuestionType.TEXT -> {
                if (options.isNotEmpty() || multiple || min != 0 || max != 0) Check.bad("form.questions.options")
                placeholder?.let { Check.line(it, MAX_PLACEHOLDER_CHARS, "form.questions.placeholder") }
            }
        }
    }
}

internal fun readOptions(array: JSONArray, where: String): List<InputOption> =
    array.objects(where).map { json ->
        val p = Obj(json, where)
        InputOption(p.str("id"), p.str("label"), p.optStr("description")).also { p.done() }
    }

/**
 * The input of a question. For a `form`, [questions] holds 1 to 4 questions and every other member is
 * empty. Only a `form` has [questions].
 */
data class Input(
    val type: InputType, val multiple: Boolean, val min: Int, val max: Int,
    val options: List<InputOption>, val placeholder: String?,
    val questions: List<FormQuestion>? = null,
) {
    fun toJson(): JSONObject = JSONObject().put("type", type.wire).put("multiple", multiple).put("min", min).put("max", max)
        .put("options", JSONArray(options.map { it.toJson() })).putOpt("placeholder", placeholder)
        .also { o -> questions?.let { o.put("questions", JSONArray(it.map { q -> q.toJson() })) } }

    companion object {
        internal fun read(o: Obj): Input {
            val type = o.enum("type", InputType.entries.toTypedArray())
            val multiple = o.bool("multiple")
            val min = o.long("min")
            val max = o.long("max")
            if (min !in 0..255 || max !in 0..255) Check.bad("input.min")
            val options = readOptions(o.arr("options"), "input.options")
            val placeholder = o.optStr("placeholder")
            val questions = o.optArr("questions")?.objects("input.questions")?.map { FormQuestion.read(Obj(it, "form.questions")) }
            o.done()
            val input = Input(type, multiple, min.toInt(), max.toInt(), options, placeholder, questions)
            input.validate()
            return input
        }
    }

    private fun validate() {
        if (type != InputType.FORM && questions != null) Check.bad("input.questions")
        when (type) {
            InputType.CHOICE -> {
                Check.choice(multiple, min, max, options, "input.options")
                if (placeholder != null) Check.bad("input.placeholder")
            }
            InputType.CONFIRM, InputType.TEXT, InputType.NONE -> {
                if (options.isNotEmpty() || multiple || min != 0 || max != 0) Check.bad("input.options")
                if (placeholder != null) {
                    if (type != InputType.TEXT) Check.bad("input.placeholder")
                    Check.line(placeholder, MAX_PLACEHOLDER_CHARS, "input.placeholder")
                }
            }
            InputType.FORM -> {
                if (options.isNotEmpty() || multiple || min != 0 || max != 0 || placeholder != null) Check.bad("input.options")
                val list = questions ?: Check.bad("input.questions")
                if (list.size !in MIN_FORM_QUESTIONS..MAX_FORM_QUESTIONS) Check.bad("input.questions")
                val seen = HashSet<String>()
                for (q in list) if (!seen.add(q.id)) Check.bad("form.questions.id")
            }
        }
    }
}

/** A suggestion of the agent or of a system. The screen shows it as not verified. */
data class Hint(val suggested: String, val confidence: Double?, val source: String) {
    fun toJson(): JSONObject = JSONObject().put("suggested", suggested).putOpt("confidence", confidence).put("source", source)

    companion object {
        internal fun read(o: Obj): Hint {
            val h = Hint(
                Check.line(o.str("suggested"), 32, "hint.suggested"),
                o.optDouble("confidence"),
                Check.line(o.str("source"), 32, "hint.source"),
            )
            o.done()
            if (h.confidence != null && !(h.confidence in 0.0..1.0)) Check.bad("hint.confidence")
            return h
        }
    }
}

data class Previous(val approvedAtMs: Long, val consumed: Boolean) {
    fun toJson(): JSONObject = JSONObject().put("approved_at_ms", approvedAtMs).put("consumed", consumed)
}

data class Follows(val code: String, val intent: Intent) {
    fun toJson(): JSONObject = JSONObject().put("code", code).put("intent", intent.wire)
}

/** A template over `op`. The phone gets the values from the native `viewResolve`. */
data class ViewSpec(val title: String, val fields: List<Pair<String, String>>, val body: String, val bodyLabel: String?) {
    fun toJson(): JSONObject = JSONObject().put("title", title)
        .put("fields", JSONArray(fields.map { JSONArray(listOf(it.first, it.second)) }))
        .put("body", body).putOpt("body_label", bodyLabel)

    companion object {
        internal fun read(o: Obj): ViewSpec {
            val title = o.str("title")
            val fields = o.arr("fields").let { a ->
                (0 until a.length()).map { n ->
                    val pair = a.get(n) as? JSONArray ?: Check.bad("approval.view")
                    if (pair.length() != 2) Check.bad("approval.view")
                    (pair.get(0) as? String ?: Check.bad("approval.view")) to (pair.get(1) as? String ?: Check.bad("approval.view"))
                }
            }
            val v = ViewSpec(title, fields, o.str("body"), o.optStr("body_label"))
            o.done()
            v.checkShape()
            return v
        }
    }

    private fun checkShape() {
        Check.line(title, MAX_VIEW_TITLE_CHARS, "approval.view")
        if (fields.size > MAX_VIEW_FIELDS) Check.bad("approval.view")
        bodyLabel?.let { Check.line(it, MAX_VIEW_LABEL_CHARS, "approval.view") }
        for ((label, ptr) in fields) {
            Check.line(label, MAX_VIEW_LABEL_CHARS, "approval.view")
            if (Check.utf8Size(ptr) > MAX_POINTER_BYTES) Check.bad("approval.view")
        }
        if (Check.utf8Size(body) > MAX_POINTER_BYTES) Check.bad("approval.view")
    }
}

/**
 * The part of a `request` that is specific to an approval. The command is the value that
 * `view.body` points to in `op`. The hash `effect` covers `service`, `action`, `resource` and
 * `op`. The explanation is not part of it. The phone computes the hash again.
 */
data class ApprovalBody(
    val code: String,
    val service: String,
    val action: String,
    val resource: String,
    val subject: String,
    val effect: String,
    /** The operation, as the frame holds it. Use [opJson] to give it to the native code. */
    val op: JSONObject,
    val view: ViewSpec,
    val explanation: String?,
    /** How to read [explanation]. Null: plain. Only with [explanation]. */
    val explanationFormat: ExplanationFormat? = null,
    /** A hint for the color of the command. It never changes the text. */
    val language: Language? = null,
    val hint: Hint?,
    val previous: Previous?,
    val follows: Follows?,
) {
    /** The text of `op` that the native code reads. The same text gives the same effect. */
    val opJson: String get() = StrictJson.compact(op)
    val viewJson: String get() = StrictJson.compact(view.toJson())

    fun toJson(): JSONObject = JSONObject().put("code", code).put("service", service).put("action", action)
        .put("resource", resource).put("subject", subject).put("effect", effect).put("op", op)
        .put("view", view.toJson()).putOpt("explanation", explanation)
        .putOpt("explanation_format", explanationFormat?.wire).putOpt("language", language?.wire)
        .putOpt("hint", hint?.toJson())
        .putOpt("previous", previous?.toJson()).putOpt("follows", follows?.toJson())

    companion object {
        internal fun read(o: Obj): ApprovalBody {
            val code = Check.code(o.str("code"), "approval.code")
            val service = Check.line(o.str("service"), 64, "approval.service")
            val action = Check.line(o.str("action"), 128, "approval.action")
            val resource = o.str("resource")
            if (resource.codePointCount(0, resource.length) > 512 || resource.any { Character.getType(it) == Character.CONTROL.toInt() }) {
                Check.bad("approval.resource")
            }
            val subject = Check.line(o.str("subject"), 128, "approval.subject")
            val effect = o.str("effect")
            if (!Check.isEffect(effect)) Check.bad("approval.effect")
            val op = Check.op(o.rawObj("op"))
            val view = ViewSpec.read(o.obj("view"))
            val explanation = o.optStr("explanation")?.let { Check.text(it, MAX_EXPLANATION_CHARS, "approval.explanation") }
            val explanationFormat = o.optEnum("explanation_format", ExplanationFormat.entries.toTypedArray())
            if (explanationFormat != null && explanation == null) Check.bad("approval.explanation_format")
            val language = o.optEnum("language", Language.entries.toTypedArray())
            val hint = o.optObj("hint")?.let { Hint.read(it) }
            val previous = o.optObj("previous")?.let { p ->
                Previous(Check.time(p.long("approved_at_ms"), "approval.previous"), p.bool("consumed")).also { p.done() }
            }
            val follows = o.optObj("follows")?.let { f ->
                Follows(Check.code(f.str("code"), "approval.follows"), f.enum("intent", Intent.entries.toTypedArray())).also { f.done() }
            }
            o.done()
            val body = ApprovalBody(
                code, service, action, resource, subject, effect, op, view, explanation, explanationFormat, language,
                hint, previous, follows,
            )
            if (Check.utf8Size(body.opJson) + Check.utf8Size(body.viewJson) > MAX_COMMAND_BYTES) Check.bad("approval.op")
            return body
        }
    }
}

/** The part of a `request` that is specific to a question. */
data class QuestionBody(
    val title: String,
    val explanation: String?,
    /** How to read [explanation]. Null: plain. Only with [explanation]. */
    val explanationFormat: ExplanationFormat? = null,
    val command: Command?,
    val input: Input,
    val hint: Hint?,
    val asOfMs: Long?,
)

// ---------------------------------------------------------------- preferences

data class QuietPrefs(val from: String, val to: String, val tz: String, val highRings: Boolean) {
    fun toJson(): JSONObject = JSONObject().put("from", from).put("to", to).put("tz", tz).put("high_rings", highRings)
}

data class DigestPrefs(val mode: DigestMode, val at: String?, val tz: String?, val everyH: Int?) {
    fun toJson(): JSONObject = JSONObject().put("mode", mode.wire).putOpt("at", at).putOpt("tz", tz).putOpt("every_h", everyH)
}

/** The preferences of the account. A missing block means: no preference, use the default of the file. */
data class Prefs(val quiet: QuietPrefs?, val digest: DigestPrefs?) {
    fun toJson(): JSONObject = JSONObject().putOpt("quiet", quiet?.toJson()).putOpt("digest", digest?.toJson())

    companion object {
        val EMPTY = Prefs(null, null)

        /** Reads the preferences from a JSON object with the rules of the protocol. */
        fun fromJson(json: JSONObject): Prefs {
            val o = Obj(json, "prefs")
            return read(o).also { o.done() }
        }

        /** Reads the members of `prefs`. The caller calls `done()` on [o] when it has no other member. */
        internal fun read(o: Obj): Prefs {
            val quiet = o.optObj("quiet")?.let { q ->
                val p = QuietPrefs(
                    Check.hhmm(q.str("from"), "prefs.quiet.from"),
                    Check.hhmm(q.str("to"), "prefs.quiet.to"),
                    Check.tz(q.str("tz"), "prefs.quiet.tz"),
                    q.bool("high_rings"),
                )
                q.done()
                if (p.from == p.to) Check.bad("prefs.quiet.to")
                p
            }
            val digest = o.optObj("digest")?.let { d ->
                val mode = d.enum("mode", DigestMode.entries.toTypedArray())
                val at = d.optStr("at")
                val tz = d.optStr("tz")
                val every = d.optLong("every_h")
                d.done()
                when (mode) {
                    DigestMode.OFF -> if (at != null || tz != null || every != null) Check.bad("prefs.digest")
                    DigestMode.DAILY -> {
                        if (at == null || tz == null || every != null) Check.bad("prefs.digest")
                        Check.hhmm(at, "prefs.digest.at")
                        Check.tz(tz, "prefs.digest.tz")
                    }
                    DigestMode.EVERY -> if (at != null || tz != null || every == null || every !in 1..168) Check.bad("prefs.digest")
                }
                DigestPrefs(mode, at, tz, every?.toInt())
            }
            return Prefs(quiet, digest)
        }
    }
}

// ---------------------------------------------------------------- server frames

/** A frame from the server to the device. */
sealed interface ServerFrame {
    fun toJson(): JSONObject
}

data class GroupInfo(val id: String, val name: String, val description: String)

data class WelcomeFrame(
    val serverName: String,
    val groups: List<GroupInfo>,
    val subscriptions: List<String>,
    val heartbeatIntervalMs: Long,
    val offlineWarningAfterMs: Long,
    val serverTimeMs: Long,
    val userName: String,
    val prefs: Prefs,
    /** The age at which the server erases the body of a final item. 0: the server has no items. */
    val bodyPurgeAfterMs: Long,
) : ServerFrame {
    /** True when the server handles items: the device sends `sync` and its outbox. */
    val itemsEnabled: Boolean get() = bodyPurgeAfterMs > 0

    override fun toJson(): JSONObject = JSONObject().put("t", "welcome").put("server_name", serverName)
        .put("groups", JSONArray(groups.map { JSONObject().put("id", it.id).put("name", it.name).put("description", it.description) }))
        .put("subscriptions", JSONArray(subscriptions)).put("heartbeat_interval_ms", heartbeatIntervalMs)
        .put("offline_warning_after_ms", offlineWarningAfterMs).put("server_time_ms", serverTimeMs)
        .put("user_name", userName).put("prefs", prefs.toJson()).put("body_purge_after_ms", bodyPurgeAfterMs)
}

/** A one-way message. The kind is always `alert` in this frame. */
data class ServerAlertFrame(
    val alertId: String, val group: String, val groupName: String, val title: String, val createdAtMs: Long,
) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "alert").put("alert_id", alertId).put("kind", "alert")
        .put("group", group).put("group_name", groupName).put("title", title).put("created_at_ms", createdAtMs)
}

/**
 * An item that waits for the owner. The same `requestId` can come again with a higher `seq`: the
 * frame is then the new state of the item. Exactly one of [question] and [approval] is set.
 */
data class RequestFrame(
    val id: String,
    val requestId: String,
    val kind: RequestKind,
    val chainId: String,
    val seq: Long,
    val from: Sender,
    val urgency: Urgency,
    val quiet: Boolean,
    val createdAtMs: Long,
    val chainCreatedAtMs: Long,
    val agentSeenAtMs: Long,
    val leaseMode: LeaseMode,
    val leaseUntilMs: Long?,
    val untilAtMs: Long?,
    val allowCustom: Boolean,
    val allowAskBack: Boolean,
    val supersedes: String?,
    val previousId: String?,
    val question: QuestionBody?,
    val approval: ApprovalBody?,
) : ServerFrame {
    override fun toJson(): JSONObject {
        val o = JSONObject().put("t", "request").put("id", id).put("request_id", requestId).put("kind", kind.wire)
            .put("chain_id", chainId).put("seq", seq).put("from", from.toJson()).put("urgency", urgency.wire)
            .put("created_at_ms", createdAtMs).put("chain_created_at_ms", chainCreatedAtMs)
            .put("agent_seen_at_ms", agentSeenAtMs).put("lease_mode", leaseMode.wire)
            .putOpt("lease_until_ms", leaseUntilMs).putOpt("until_at_ms", untilAtMs)
            .put("allow_custom", allowCustom).put("allow_ask_back", allowAskBack)
            .putOpt("supersedes", supersedes).putOpt("previous_id", previousId)
        if (quiet) o.put("quiet", true)
        question?.let { q ->
            o.put("title", q.title).putOpt("explanation", q.explanation)
                .putOpt("explanation_format", q.explanationFormat?.wire).putOpt("command", q.command?.toJson())
                .put("input", q.input.toJson()).putOpt("hint", q.hint?.toJson()).putOpt("as_of_ms", q.asOfMs)
        }
        approval?.let { o.put("approval", it.toJson()) }
        return o
    }
}

/** A turn in the thread of an item. An agent turn has `explanation`, `command` or both. A user turn has `text` only. */
data class MessageFrame(
    val id: String,
    val requestId: String,
    val seq: Long,
    val by: Author,
    val createdAtMs: Long,
    val explanation: String?,
    /** How to read [explanation]. Null: plain. Only with [explanation]. */
    val explanationFormat: ExplanationFormat? = null,
    val command: Command?,
    val text: String?,
) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "message").put("id", id).put("request_id", requestId)
        .put("seq", seq).put("by", by.wire).put("created_at_ms", createdAtMs).putOpt("explanation", explanation)
        .putOpt("explanation_format", explanationFormat?.wire).putOpt("command", command?.toJson()).putOpt("text", text)
}

data class SettledBy(val via: Channel, val deviceName: String?) {
    fun toJson(): JSONObject = JSONObject().put("via", via.wire).putOpt("device_name", deviceName)
}

/** An item is final. */
data class SettledFrame(
    val id: String,
    val requestId: String,
    val state: ItemState,
    /** A token. A device shows a token that it does not know as it is. */
    val reason: String,
    val by: SettledBy?,
    val supersededBy: String?,
    val decision: Decision?,
    val atMs: Long,
) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "settled").put("id", id).put("request_id", requestId)
        .put("state", state.wire).put("reason", reason).putOpt("by", by?.toJson()).putOpt("superseded_by", supersededBy)
        .putOpt("decision", decision?.wire).put("at_ms", atMs)
}

/** The open items of the owner, as numbers. It has no body. */
data class DigestFrame(val id: String, val count: Long, val oldestCreatedAtMs: Long, val ids: List<String>) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "digest").put("id", id).put("count", count)
        .put("oldest_created_at_ms", oldestCreatedAtMs).put("ids", JSONArray(ids))
}

/** The result of an `answer` or a `reply`. It has no delivery id: the device does not acknowledge it. */
data class AnswerResultFrame(
    val requestId: String, val clientMsgId: String, val status: AnswerStatus, val state: ItemState?,
) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "answer_result").put("request_id", requestId)
        .put("client_msg_id", clientMsgId).put("status", status.wire).putOpt("state", state?.wire)
}

data class SubscriptionsFrame(val subscriptions: List<String>) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "subscriptions").put("subscriptions", JSONArray(subscriptions))
}

data class PongFrame(val nonce: Long) : ServerFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "pong").put("nonce", nonce)
}

data class ErrorFrame(val code: String, val message: String) : ServerFrame {
    /** The server does not handle this frame: the items are not enabled. */
    val isNotEnabled: Boolean get() = code == "not_enabled"

    override fun toJson(): JSONObject = JSONObject().put("t", "error").put("code", code).put("message", message)
}

object ServerFrames {
    fun decode(bytes: ByteArray): ServerFrame = decode(StrictJson.decodeUtf8(bytes))

    fun decode(text: String): ServerFrame {
        val json = StrictJson.parseObject(text)
        val o = Obj(json, "frame")
        return when (val t = o.str("t")) {
            "welcome" -> welcome(o)
            "alert" -> alert(o)
            "request" -> request(o)
            "message" -> message(o)
            "settled" -> settled(o)
            "digest" -> digest(o)
            "answer_result" -> answerResult(o)
            "subscriptions" -> SubscriptionsFrame(o.arr("subscriptions").strings("subscriptions")).also { o.done() }
            "pong" -> PongFrame(o.unsigned("nonce")).also { o.done() }
            "error" -> ErrorFrame(o.str("code"), o.str("message")).also { o.done() }
            else -> Check.bad("unknown frame $t".take(40))
        }
    }

    private fun welcome(o: Obj): WelcomeFrame {
        val groups = o.arr("groups").objects("welcome.groups").map {
            val g = Obj(it, "welcome.groups")
            GroupInfo(g.str("id"), g.str("name"), g.str("description")).also { g.done() }
        }
        val prefsObj = o.obj("prefs")
        val prefs = Prefs.read(prefsObj).also { prefsObj.done() }
        val f = WelcomeFrame(
            serverName = o.str("server_name"),
            groups = groups,
            subscriptions = o.arr("subscriptions").strings("welcome.subscriptions"),
            heartbeatIntervalMs = o.unsigned("heartbeat_interval_ms"),
            offlineWarningAfterMs = o.unsigned("offline_warning_after_ms"),
            serverTimeMs = o.long("server_time_ms"),
            userName = o.str("user_name"),
            prefs = prefs,
            bodyPurgeAfterMs = o.unsigned("body_purge_after_ms"),
        )
        o.done()
        return f
    }

    private fun alert(o: Obj): ServerAlertFrame {
        val alertId = Check.id(o.str("alert_id"), "alert.alert_id")
        if (o.str("kind") != "alert") Check.bad("alert.kind")
        val f = ServerAlertFrame(
            alertId, o.str("group"), o.str("group_name"),
            Check.line(o.str("title"), MAX_TITLE_CHARS, "title"),
            Check.time(o.long("created_at_ms"), "alert.created_at_ms"),
        )
        o.done()
        return f
    }

    private fun request(o: Obj): RequestFrame {
        val id = Check.id(o.str("id"), "request.id")
        val requestId = Check.id(o.str("request_id"), "request.request_id")
        val kind = o.enum("kind", RequestKind.entries.toTypedArray())
        val chainId = Check.id(o.str("chain_id"), "request.chain_id")
        val seq = o.unsigned("seq")
        val from = Sender.read(o.obj("from"))
        val urgency = o.enum("urgency", Urgency.entries.toTypedArray())
        val quiet = o.optBool("quiet", false)
        val created = Check.time(o.long("created_at_ms"), "request.created_at_ms")
        val chainCreated = Check.time(o.long("chain_created_at_ms"), "request.chain_created_at_ms")
        val agentSeen = Check.time(o.long("agent_seen_at_ms"), "request.agent_seen_at_ms")
        val leaseMode = o.enum("lease_mode", LeaseMode.entries.toTypedArray())
        val leaseUntil = o.optLong("lease_until_ms")
        val untilAt = o.optLong("until_at_ms")
        val allowCustom = o.bool("allow_custom")
        val allowAskBack = o.bool("allow_ask_back")
        val supersedes = o.optStr("supersedes")?.let { Check.id(it, "request.supersedes") }
        val previousId = o.optStr("previous_id")?.let { Check.id(it, "request.previous_id") }
        when {
            leaseMode == LeaseMode.LEASE && leaseUntil != null && untilAt == null -> Check.time(leaseUntil, "request.lease")
            leaseMode == LeaseMode.UNTIL && leaseUntil == null && untilAt != null -> Check.time(untilAt, "request.lease")
            else -> Check.bad("request.lease_mode")
        }
        if (from.interaction == Interaction.ONE_SHOT && (allowCustom || allowAskBack)) Check.bad("request.allow")

        val hasQuestionMembers = o.has("title") || o.has("explanation") || o.has("explanation_format") ||
            o.has("command") || o.has("input") || o.has("hint") || o.has("as_of_ms")
        var question: QuestionBody? = null
        var approval: ApprovalBody? = null
        when (kind) {
            RequestKind.QUESTION -> {
                if (o.has("approval")) Check.bad("request.approval")
                if (!o.has("title") || !o.has("input")) Check.bad("request.title")
                val title = Check.title(o.str("title"), "title")
                val explanation = o.optStr("explanation")?.let { Check.text(it, MAX_EXPLANATION_CHARS, "request.explanation") }
                val explanationFormat = o.optEnum("explanation_format", ExplanationFormat.entries.toTypedArray())
                if (explanationFormat != null && explanation == null) Check.bad("request.explanation_format")
                val command = o.optObj("command")?.let { Command.read(it) }
                val input = Input.read(o.obj("input"))
                if (input.type == InputType.TEXT && allowCustom) Check.bad("request.allow_custom")
                if (input.type == InputType.FORM) {
                    // A closed automatic system gives one decision: it has no form.
                    if (from.interaction == Interaction.ONE_SHOT) Check.bad("request.form")
                    // `allow_custom` adds a row "Other" to each choice question: a form with none has no use for it.
                    if (allowCustom && input.questions.orEmpty().none { it.type == FormQuestionType.CHOICE }) {
                        Check.bad("request.allow_custom")
                    }
                }
                if (input.type == InputType.NONE && (allowCustom || allowAskBack)) Check.bad("request.allow")
                val hint = o.optObj("hint")?.let { Hint.read(it) }
                val asOf = o.optLong("as_of_ms")?.let { Check.time(it, "request.as_of_ms") }
                question = QuestionBody(title, explanation, explanationFormat, command, input, hint, asOf)
            }
            RequestKind.APPROVAL -> {
                if (!o.has("approval") || hasQuestionMembers) Check.bad("request.approval")
                approval = ApprovalBody.read(o.obj("approval"))
            }
        }
        o.done()
        return RequestFrame(
            id, requestId, kind, chainId, seq, from, urgency, quiet, created, chainCreated, agentSeen, leaseMode,
            leaseUntil, untilAt, allowCustom, allowAskBack, supersedes, previousId, question, approval,
        )
    }

    private fun message(o: Obj): MessageFrame {
        val id = Check.id(o.str("id"), "message.id")
        val requestId = Check.id(o.str("request_id"), "message.request_id")
        val seq = o.unsigned("seq")
        if (seq < 1) Check.bad("message.seq")
        val by = o.enum("by", Author.entries.toTypedArray())
        val created = Check.time(o.long("created_at_ms"), "message.created_at_ms")
        val explanation = o.optStr("explanation")
        val explanationFormat = o.optEnum("explanation_format", ExplanationFormat.entries.toTypedArray())
        val command = o.optObj("command")?.let { Command.read(it) }
        val text = o.optStr("text")
        o.done()
        when (by) {
            Author.AGENT -> {
                if (text != null || (explanation == null && command == null)) Check.bad("message.text")
                explanation?.let { Check.text(it, MAX_EXPLANATION_CHARS, "message.explanation") }
                if (explanationFormat != null && explanation == null) Check.bad("message.explanation_format")
            }
            Author.USER -> {
                if (explanation != null || explanationFormat != null || command != null) Check.bad("message.explanation")
                Check.text(text ?: Check.bad("message.text"), MAX_USER_TEXT_CHARS, "message.text")
            }
        }
        return MessageFrame(id, requestId, seq, by, created, explanation, explanationFormat, command, text)
    }

    private fun settled(o: Obj): SettledFrame {
        val id = Check.id(o.str("id"), "settled.id")
        val requestId = Check.id(o.str("request_id"), "settled.request_id")
        val state = o.enum("state", ItemState.entries.toTypedArray())
        if (!state.isFinal) Check.bad("settled.state")
        val reason = Check.token(o.str("reason"), "settled.reason")
        val by = o.optObj("by")?.let { b ->
            val via = b.enum("via", Channel.entries.toTypedArray())
            val name = b.optStr("device_name")
            b.done()
            if (name != null) {
                if (via != Channel.PHONE) Check.bad("settled.by.device_name")
                Check.line(name, 64, "settled.by.device_name")
            }
            SettledBy(via, name)
        }
        val supersededBy = o.optStr("superseded_by")?.let { Check.id(it, "settled.superseded_by") }
        val decision = o.optEnum("decision", Decision.entries.toTypedArray())
        val at = Check.time(o.long("at_ms"), "settled.at_ms")
        o.done()
        if (supersededBy != null && state != ItemState.SUPERSEDED) Check.bad("settled.superseded_by")
        if (decision != null && state != ItemState.ANSWERED) Check.bad("settled.decision")
        return SettledFrame(id, requestId, state, reason, by, supersededBy, decision, at)
    }

    private fun digest(o: Obj): DigestFrame {
        val id = Check.id(o.str("id"), "digest.id")
        val count = o.unsigned("count")
        val oldest = Check.time(o.long("oldest_created_at_ms"), "digest.oldest_created_at_ms")
        val ids = o.arr("ids").strings("digest.ids")
        o.done()
        if (count < 1 || count > 0xFFFF_FFFFL || ids.size > MAX_DIGEST_IDS || ids.size > count) Check.bad("digest.count")
        ids.forEach { Check.id(it, "digest.ids") }
        return DigestFrame(id, count, oldest, ids)
    }

    private fun answerResult(o: Obj): AnswerResultFrame {
        val f = AnswerResultFrame(
            Check.id(o.str("request_id"), "answer_result.request_id"),
            Check.clientMsgId(o.str("client_msg_id")),
            o.enum("status", AnswerStatus.entries.toTypedArray()),
            o.optEnum("state", ItemState.entries.toTypedArray()),
        )
        o.done()
        return f
    }
}

// ---------------------------------------------------------------- client frames

/** A frame from the device to the server. Build it, then call [ClientFrames.encode]. */
sealed interface ClientFrame {
    fun toJson(): JSONObject

    /** Checks the rules of the protocol. [ClientFrames.encode] calls it. */
    fun validate()
}

object ReadyFrame : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "ready")
    override fun validate() {}
}

/** The receipt of a delivery: the `alert_id` of an alert, or the `id` of another frame. */
data class AckFrame(val id: String) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "ack").put("id", id).put("stage", "received")
    override fun validate() { Check.id(id, "ack.id") }
}

data class SubscribeFrame(val group: String) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "subscribe").put("group", group)
    override fun validate() {}
}

data class UnsubscribeFrame(val group: String) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "unsubscribe").put("group", group)
    override fun validate() {}
}

data class PingFrame(val nonce: Long) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "ping").put("nonce", nonce)
    override fun validate() { if (nonce < 0) Check.bad("ping.nonce") }
}

/**
 * The answer to one question of a form: [choice] (the ticked option ids) or [text], never both. For a choice
 * question, [text] is the free answer "Other": it is exclusive of the ticks.
 */
data class FormAnswer(val questionId: String, val choice: List<String>? = null, val text: String? = null) {
    fun toJson(): JSONObject = JSONObject().put("question_id", questionId)
        .putOpt("choice", choice?.let { JSONArray(it) }).putOpt("text", text)

    internal fun validate() {
        if (!Check.optionId(questionId)) Check.bad("answer.answers.question_id")
        when {
            choice != null && text == null -> {
                if (choice.isEmpty() || choice.size > MAX_OPTIONS) Check.bad("answer.answers.choice")
                val seen = HashSet<String>()
                if (!choice.all { Check.optionId(it) && seen.add(it) }) Check.bad("answer.answers.choice")
            }
            choice == null && text != null -> {
                Check.text(text, MAX_USER_TEXT_CHARS, "answer.answers.text")
                if (text.contains('\u0000')) Check.bad("answer.answers.text")
            }
            else -> Check.bad("answer.answers")
        }
    }

    companion object {
        internal fun read(o: Obj): FormAnswer {
            val a = FormAnswer(o.str("question_id"), o.optArr("choice")?.strings("answer.answers.choice"), o.optStr("text"))
            o.done()
            return a
        }
    }
}

/**
 * The only answer of an item. It has no signature: the proof is the authenticated session and,
 * for an approval, the hash [shownEffect] that the phone computed from the `op` that it received.
 */
data class AnswerFrame(
    val requestId: String,
    val clientMsgId: String,
    val action: Action,
    val shownEffect: String? = null,
    val choice: List<String>? = null,
    val text: String? = null,
    val intent: Intent? = null,
    /** The answers to a form, one for each question, 1 to 4. Only with `accept`. It excludes the other members. */
    val answers: List<FormAnswer>? = null,
) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "answer").put("request_id", requestId)
        .put("client_msg_id", clientMsgId).put("action", action.wire).putOpt("shown_effect", shownEffect)
        .putOpt("choice", choice?.let { JSONArray(it) }).putOpt("text", text).putOpt("intent", intent?.wire)
        .also { o -> answers?.let { o.put("answers", JSONArray(it.map { a -> a.toJson() })) } }

    override fun validate() {
        Check.id(requestId, "answer.request_id")
        Check.clientMsgId(clientMsgId)
        if (shownEffect != null && (action != Action.ACCEPT || !Check.isEffect(shownEffect))) Check.bad("answer.shown_effect")
        if (choice != null) {
            if (choice.size > MAX_OPTIONS || !choice.all { Check.optionId(it) }) Check.bad("answer.choice")
            if (action == Action.DECLINE) Check.bad("answer.choice")
        }
        if (text != null) {
            Check.text(text, MAX_USER_TEXT_CHARS, "answer.text")
            if (text.contains('\u0000')) Check.bad("answer.text")
        }
        if (intent != null && (text == null || action != Action.DECLINE)) Check.bad("answer.intent")
        if (answers != null) {
            if (action != Action.ACCEPT || choice != null || text != null || intent != null || shownEffect != null) {
                Check.bad("answer.answers")
            }
            if (answers.size !in MIN_FORM_QUESTIONS..MAX_FORM_QUESTIONS) Check.bad("answer.answers")
            val seen = HashSet<String>()
            for (a in answers) {
                a.validate()
                if (!seen.add(a.questionId)) Check.bad("answer.answers.question_id")
            }
        }
    }
}

/** The owner asks the agent a question. The item stays open and becomes `replied`. */
data class ReplyFrame(val requestId: String, val clientMsgId: String, val text: String) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "reply").put("request_id", requestId)
        .put("client_msg_id", clientMsgId).put("text", text)

    override fun validate() {
        Check.id(requestId, "reply.request_id")
        Check.clientMsgId(clientMsgId)
        Check.text(text, MAX_USER_TEXT_CHARS, "reply.text")
        if (text.contains('\u0000')) Check.bad("reply.text")
    }
}

data class RefreshFrame(val requestId: String) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "refresh").put("request_id", requestId)
    override fun validate() { Check.id(requestId, "refresh.request_id") }
}

data class OpenRef(val requestId: String, val seq: Long)

/** The items that the device holds open, with the last turn that it knows. */
data class SyncFrame(val open: List<OpenRef>) : ClientFrame {
    override fun toJson(): JSONObject = JSONObject().put("t", "sync")
        .put("open", JSONArray(open.map { JSONObject().put("request_id", it.requestId).put("seq", it.seq) }))

    override fun validate() {
        if (open.size > MAX_SYNC_ENTRIES) Check.bad("sync.open")
        open.forEach { Check.id(it.requestId, "sync.open"); if (it.seq < 0) Check.bad("sync.open") }
    }
}

/** Replaces the preferences of the account. */
data class SetPrefsFrame(val prefs: Prefs) : ClientFrame {
    override fun toJson(): JSONObject = prefs.toJson().put("t", "set_prefs")

    override fun validate() {
        // The rules are the rules of the reader: a frame that does not read back is not valid.
        Prefs.read(Obj(prefs.toJson(), "prefs"))
    }
}

object ClientFrames {
    /** Reads a frame of the device. The app reads its own frames from the outbox and in tests. */
    fun decode(bytes: ByteArray): ClientFrame = decode(StrictJson.decodeUtf8(bytes))

    fun decode(text: String): ClientFrame {
        val o = Obj(StrictJson.parseObject(text), "frame")
        val frame: ClientFrame = when (val t = o.str("t")) {
            "ready" -> ReadyFrame
            "ack" -> {
                val id = o.str("id")
                if (o.str("stage") != "received") Check.bad("ack.stage")
                AckFrame(id)
            }
            "subscribe" -> SubscribeFrame(o.str("group"))
            "unsubscribe" -> UnsubscribeFrame(o.str("group"))
            "ping" -> PingFrame(o.long("nonce"))
            "answer" -> AnswerFrame(
                o.str("request_id"), o.str("client_msg_id"), o.enum("action", Action.entries.toTypedArray()),
                o.optStr("shown_effect"), o.optArr("choice")?.strings("answer.choice"), o.optStr("text"),
                o.optEnum("intent", Intent.entries.toTypedArray()),
                o.optArr("answers")?.objects("answer.answers")?.map { FormAnswer.read(Obj(it, "answer.answers")) },
            )
            "reply" -> ReplyFrame(o.str("request_id"), o.str("client_msg_id"), o.str("text"))
            "refresh" -> RefreshFrame(o.str("request_id"))
            "sync" -> SyncFrame(
                o.arr("open").objects("sync.open").map {
                    val r = Obj(it, "sync.open")
                    OpenRef(r.str("request_id"), r.unsigned("seq")).also { r.done() }
                },
            )
            "set_prefs" -> SetPrefsFrame(Prefs.read(o))
            else -> Check.bad("unknown frame $t".take(40))
        }
        o.done()
        frame.validate()
        return frame
    }

    /** Checks a frame and writes it as one Noise plain text. */
    fun encode(frame: ClientFrame): ByteArray {
        frame.validate()
        val bytes = StrictJson.compact(frame.toJson()).toByteArray(Charsets.UTF_8)
        if (bytes.size > MAX_PLAINTEXT) Check.bad("frame too large for one noise message")
        return bytes
    }
}
