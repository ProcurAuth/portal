package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.FormAnswer
import app.portalinbox.proto.FormQuestionType
import app.portalinbox.proto.InputOption
import app.portalinbox.proto.InputType
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.RequestKind

/** How a block of the sheet takes its answer. */
enum class BlockKind {
    /** One option of a list. */
    RADIO,

    /** Several options of a list, `min` to `max`. */
    CHECKBOX,

    /** Yes or no: a radio with the two fixed options. */
    CONFIRM,

    /** A free text. */
    TEXT,
}

/**
 * One question that the owner answers. A question of the type `choice`, `confirm` or `text` has one block with no id.
 * A form has one block for each of its 1 to 4 questions. All text in a block (header, text, labels) comes from the agent.
 */
data class SheetBlock(
    /** The id of the question of a form, or null for a single question. */
    val id: String?,
    val header: String?,
    val text: String?,
    val kind: BlockKind,
    val options: List<InputOption>,
    val min: Int,
    val max: Int,
    /** The row "Other answer" shows in this block (the flag `allow_custom` and a choice block). */
    val otherAllowed: Boolean,
    val placeholder: String?,
) {
    val isChoice: Boolean get() = kind != BlockKind.TEXT
}

/** The questions of a request, in the order on the screen. */
data class Sheet(val blocks: List<SheetBlock>, val isForm: Boolean)

/**
 * What the owner entered in one block. A radio keeps at most one tick. With [otherOn] the free text [otherText] is the
 * answer and the block holds no tick: the free answer excludes the ticks.
 */
data class BlockDraft(
    val ticked: List<String> = emptyList(),
    val otherOn: Boolean = false,
    val otherText: String = "",
    /** The text of a block of the kind TEXT. */
    val text: String = "",
)

data class SheetDraft(val blocks: List<BlockDraft>)

/** Where one block stands. */
sealed interface BlockStatus {
    /** Nothing is chosen or written. */
    data object Empty : BlockStatus

    data object Complete : BlockStatus

    /** The entry breaks a rule of the server. [reason] is a sentence for the owner. */
    data class Invalid(val reason: String) : BlockStatus
}

/** The result of the build of an `answer` frame. */
sealed interface Built {
    data class Frame(val frame: AnswerFrame) : Built

    /** [blocks] are the numbers (from 0) of the blocks that have no valid answer. */
    data class Refused(val message: String, val blocks: List<Int>) : Built
}

/**
 * The answer of the owner to a question or to a form. Pure Kotlin. It has the rules of the server: those of the
 * `validate_form_answer` of the core crate, and the same ones for a single question, so the phone shows a mistake before
 * it sends and the server finds nothing to refuse.
 *
 * Rules:
 * - A radio takes one tick. A checkbox takes `min` to `max` ticks. A tick names a known option, once.
 * - The free answer ("Other answer") exists only when the request has `allow_custom`. It is exclusive of the ticks: choosing
 *   it clears them, and ticking an option clears it. `min` and `max` count the ticks only.
 * - A text answer has 1 to 2000 characters and no NUL. A text of only spaces is no answer. The text is not changed.
 * - Every block of a form needs an answer. A decline carries no text.
 */
object AnswerSheet {
    /** The server takes 2000 characters of free text for a question (SPEC 3.6.23). The core takes more for a form: the phone keeps the lower limit. */
    const val MAX_TEXT_CHARS = 2000

    val CONFIRM_OPTIONS: List<InputOption> = listOf(
        InputOption("yes", "Yes", null),
        InputOption("no", "No", null),
    )

    /**
     * The sheet of a request, or null when the request takes no answer on a sheet: an approval, or a message with no
     * answer (`none`). [FlagsMatrix] decides if the row "Other answer" exists.
     */
    fun of(r: RequestFrame): Sheet? {
        val q = r.question ?: return null
        if (r.kind != RequestKind.QUESTION) return null
        val custom = FlagsMatrix.rows(r).otherAnswer
        val input = q.input
        return when (input.type) {
            InputType.CHOICE -> Sheet(
                listOf(
                    SheetBlock(
                        null, null, null, if (input.multiple) BlockKind.CHECKBOX else BlockKind.RADIO,
                        input.options, input.min, input.max, custom, null,
                    ),
                ),
                false,
            )
            InputType.CONFIRM -> Sheet(listOf(SheetBlock(null, null, null, BlockKind.CONFIRM, CONFIRM_OPTIONS, 1, 1, custom, null)), false)
            InputType.TEXT -> Sheet(listOf(SheetBlock(null, null, null, BlockKind.TEXT, emptyList(), 0, 0, false, input.placeholder)), false)
            InputType.NONE -> null
            InputType.FORM -> Sheet(
                input.questions.orEmpty().map { f ->
                    when (f.type) {
                        FormQuestionType.CHOICE -> SheetBlock(
                            f.id, f.header, f.text, if (f.multiple) BlockKind.CHECKBOX else BlockKind.RADIO,
                            f.options, f.min, f.max, custom, null,
                        )
                        FormQuestionType.TEXT -> SheetBlock(f.id, f.header, f.text, BlockKind.TEXT, emptyList(), 0, 0, false, f.placeholder)
                    }
                },
                true,
            )
        }
    }

    fun empty(sheet: Sheet): SheetDraft = SheetDraft(sheet.blocks.map { BlockDraft() })

    // ------------------------------------------------------------ what the owner does

    /**
     * A tap on an option. A radio or a confirm gets this option alone. A checkbox flips the option. A tick clears the free
     * answer. A tick past `max` does not change the draft: the screen says that the limit is reached.
     */
    fun tap(sheet: Sheet, draft: SheetDraft, block: Int, optionId: String): SheetDraft {
        val b = sheet.blocks[block]
        if (!b.isChoice || b.options.none { it.id == optionId }) return draft
        val d = draft.blocks[block]
        val next = when (b.kind) {
            BlockKind.RADIO, BlockKind.CONFIRM -> d.copy(ticked = listOf(optionId), otherOn = false)
            BlockKind.CHECKBOX -> when {
                optionId in d.ticked -> d.copy(ticked = d.ticked - optionId)
                d.ticked.size >= b.max -> return draft
                else -> d.copy(ticked = d.ticked + optionId, otherOn = false)
            }
            BlockKind.TEXT -> return draft
        }
        return replace(draft, block, next)
    }

    /** True when a tap on [optionId] would be refused because `max` ticks are already set. */
    fun atMax(sheet: Sheet, draft: SheetDraft, block: Int, optionId: String): Boolean {
        val b = sheet.blocks[block]
        val d = draft.blocks[block]
        return b.kind == BlockKind.CHECKBOX && optionId !in d.ticked && d.ticked.size >= b.max
    }

    /** A tap on the row "Other answer". It clears the ticks. For a checkbox, a second tap turns the row off. */
    fun tapOther(sheet: Sheet, draft: SheetDraft, block: Int): SheetDraft {
        val b = sheet.blocks[block]
        if (!b.otherAllowed) return draft
        val d = draft.blocks[block]
        val next = if (d.otherOn && b.kind == BlockKind.CHECKBOX) d.copy(otherOn = false)
        else d.copy(ticked = emptyList(), otherOn = true)
        return replace(draft, block, next)
    }

    /** The owner writes in the field of "Other answer". Writing selects the row, and so clears the ticks. */
    fun typeOther(sheet: Sheet, draft: SheetDraft, block: Int, text: String): SheetDraft {
        if (!sheet.blocks[block].otherAllowed) return draft
        return replace(draft, block, draft.blocks[block].copy(ticked = emptyList(), otherOn = true, otherText = text))
    }

    /** The owner writes the text of a block of the kind TEXT. */
    fun typeText(sheet: Sheet, draft: SheetDraft, block: Int, text: String): SheetDraft {
        if (sheet.blocks[block].kind != BlockKind.TEXT) return draft
        return replace(draft, block, draft.blocks[block].copy(text = text))
    }

    private fun replace(draft: SheetDraft, block: Int, next: BlockDraft) =
        SheetDraft(draft.blocks.mapIndexed { i, d -> if (i == block) next else d })

    // ------------------------------------------------------------ the rules of the server

    private fun textProblem(text: String): String? = when {
        text.contains('\u0000') -> "The text has a character that is not allowed."
        text.codePointCount(0, text.length) > MAX_TEXT_CHARS -> "The text is too long. Use $MAX_TEXT_CHARS characters at most."
        else -> null
    }

    /** The number of ticks that the block needs, in words: "Choose one.", "Choose at least 2.", "Choose 2 to 3.". */
    fun chooseLine(b: SheetBlock): String = when (b.kind) {
        BlockKind.TEXT -> "Write your answer."
        BlockKind.RADIO, BlockKind.CONFIRM -> "Choose one."
        BlockKind.CHECKBOX -> when {
            b.min == b.max -> "Choose exactly ${b.min}."
            b.max >= b.options.size -> "Choose at least ${b.min}."
            else -> "Choose ${b.min} to ${b.max}."
        }
    }

    fun status(sheet: Sheet, draft: SheetDraft, block: Int): BlockStatus {
        val b = sheet.blocks[block]
        val d = draft.blocks[block]
        if (b.kind == BlockKind.TEXT) {
            if (d.text.isBlank()) return BlockStatus.Empty
            return textProblem(d.text)?.let { BlockStatus.Invalid(it) } ?: BlockStatus.Complete
        }
        if (d.otherOn && b.otherAllowed) {
            if (d.ticked.isNotEmpty()) return BlockStatus.Invalid("A free answer excludes the options.")
            if (d.otherText.isBlank()) return BlockStatus.Empty
            return textProblem(d.otherText)?.let { BlockStatus.Invalid(it) } ?: BlockStatus.Complete
        }
        if (d.ticked.isEmpty()) return BlockStatus.Empty
        val known = b.options.map { it.id }.toSet()
        if (d.ticked.any { it !in known } || d.ticked.toSet().size != d.ticked.size) return BlockStatus.Invalid("An option is not valid.")
        if (d.ticked.size < b.min || d.ticked.size > b.max) {
            return BlockStatus.Invalid(if (d.ticked.size < b.min) "Choose at least ${b.min}." else "Choose at most ${b.max}.")
        }
        return BlockStatus.Complete
    }

    /** True when the block has a valid answer. */
    fun complete(sheet: Sheet, draft: SheetDraft, block: Int): Boolean = status(sheet, draft, block) == BlockStatus.Complete

    /** The numbers of the blocks that have no valid answer, from 0. */
    fun missing(sheet: Sheet, draft: SheetDraft): List<Int> = sheet.blocks.indices.filter { !complete(sheet, draft, it) }

    // ------------------------------------------------------------ the frame

    /** The words of the line that tells why Send is off: "Question 2 has no answer." */
    fun missingLine(sheet: Sheet, draft: SheetDraft): String? {
        val m = missing(sheet, draft)
        if (m.isEmpty()) return null
        if (!sheet.isForm) {
            return when (val s = status(sheet, draft, 0)) {
                is BlockStatus.Invalid -> s.reason
                else -> if (sheet.blocks[0].kind == BlockKind.TEXT) "Write your answer to turn Send on." else "Choose an answer to turn Send on."
            }
        }
        val names = m.map { it + 1 }
        val list = when (names.size) {
            1 -> "Question ${names[0]} has"
            2 -> "Questions ${names[0]} and ${names[1]} have"
            else -> "Questions " + names.dropLast(1).joinToString(", ") + " and " + names.last() + " have"
        }
        return "Send is off. $list no valid answer."
    }

    /**
     * The `answer` frame (accept) for the draft: `choice` for the ticks, `text` for a free answer or a text question, `answers`
     * for a form. The ticks go in the order of the options, not of the taps. A draft that breaks a rule gives [Built.Refused].
     */
    fun build(sheet: Sheet, draft: SheetDraft, requestId: String, clientMsgId: String): Built {
        val missing = missing(sheet, draft)
        if (missing.isNotEmpty()) {
            return Built.Refused(missingLine(sheet, draft) ?: "The answer is not complete.", missing)
        }
        val parts = sheet.blocks.indices.map { part(sheet.blocks[it], draft.blocks[it]) }
        if (!sheet.isForm) {
            val (choice, text) = parts[0]
            return Built.Frame(AnswerFrame(requestId, clientMsgId, Action.ACCEPT, choice = choice, text = text))
        }
        val answers = sheet.blocks.indices.map { FormAnswer(sheet.blocks[it].id!!, parts[it].first, parts[it].second) }
        return Built.Frame(AnswerFrame(requestId, clientMsgId, Action.ACCEPT, answers = answers))
    }

    private fun part(b: SheetBlock, d: BlockDraft): Pair<List<String>?, String?> = when {
        b.kind == BlockKind.TEXT -> null to d.text
        d.otherOn && b.otherAllowed -> null to d.otherText
        else -> {
            val order = b.options.map { it.id }
            d.ticked.sortedBy { order.indexOf(it) } to null
        }
    }

    /** The decline of a question: no text, no choice. The owner can always decline. */
    fun decline(requestId: String, clientMsgId: String): AnswerFrame = AnswerFrame(requestId, clientMsgId, Action.DECLINE)
}
