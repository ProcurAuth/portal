package app.portalinbox.data

import app.portalinbox.proto.FormQuestionType
import app.portalinbox.proto.InputType
import app.portalinbox.proto.Interaction
import app.portalinbox.proto.RequestFrame

/**
 * The rows that the screen of a question shows under the options, from the two flags of the agent (DIAL 1.4.2, 1.6, 4.2).
 * One pure function decides: the screen draws what [Rows] says and nothing more.
 *
 * | `allow_custom` | `allow_ask_back` | Rows under the options |
 * |---|---|---|
 * | false | false | none: the question is closed |
 * | true | false | "Other answer" with a field |
 * | false | true | "Ask the agent" |
 * | true | true | both rows |
 *
 * Rules of validity. A combination that breaks one gives [Rows.invalid] and no row at all: the phone never invents an
 * escape that the server refuses.
 * - A `text` question has no row "Other answer": its field is already the answer. `allow_custom` on it is not valid.
 * - A `none` question (a message with no answer) takes no flag.
 * - A key `one_shot` takes no flag, and only the types `choice` and `confirm`. It has no discussion.
 * - A form has the row "Other answer" in each of its choice questions. `allow_custom` on a form with no choice question is not valid.
 */
object FlagsMatrix {
    enum class Invalid(val line: String) {
        TEXT_WITH_CUSTOM("A text question cannot also take another answer."),
        NONE_WITH_FLAG("A message with no answer cannot take an answer row."),
        ONE_SHOT_WITH_FLAG("An automatic system cannot take a text or a discussion."),
        ONE_SHOT_INPUT("An automatic system can only ask a choice or a yes or no."),
        FORM_CUSTOM_WITHOUT_CHOICE("A form with no choice cannot take another answer."),
    }

    data class Rows(
        /** The row "Other answer" shows, in each choice block. */
        val otherAnswer: Boolean,
        /** The row "Ask the agent" shows: it opens the discussion. */
        val askAgent: Boolean,
        val invalid: Invalid?,
    ) {
        /** True when the question has no escape row: the owner can only choose, write the answer, or decline. */
        val closed: Boolean get() = !otherAnswer && !askAgent
    }

    private val CLOSED = Rows(false, false, null)

    /**
     * @param formHasChoice for a form: at least one of its questions is a choice. Ignored for the other types.
     */
    fun rows(type: InputType, allowCustom: Boolean, allowAskBack: Boolean, interaction: Interaction, formHasChoice: Boolean = true): Rows {
        fun bad(why: Invalid) = Rows(false, false, why)
        if (interaction == Interaction.ONE_SHOT) {
            return when {
                allowCustom || allowAskBack -> bad(Invalid.ONE_SHOT_WITH_FLAG)
                type != InputType.CHOICE && type != InputType.CONFIRM -> bad(Invalid.ONE_SHOT_INPUT)
                else -> CLOSED
            }
        }
        return when (type) {
            InputType.NONE -> if (allowCustom || allowAskBack) bad(Invalid.NONE_WITH_FLAG) else CLOSED
            InputType.TEXT -> if (allowCustom) bad(Invalid.TEXT_WITH_CUSTOM) else Rows(false, allowAskBack, null)
            InputType.FORM -> if (allowCustom && !formHasChoice) bad(Invalid.FORM_CUSTOM_WITHOUT_CHOICE) else Rows(allowCustom, allowAskBack, null)
            InputType.CHOICE, InputType.CONFIRM -> Rows(allowCustom, allowAskBack, null)
        }
    }

    /** The rows of a request. An approval has no row of a question: it gives the closed rows. */
    fun rows(r: RequestFrame): Rows {
        val input = r.question?.input ?: return CLOSED
        val hasChoice = input.questions.orEmpty().any { it.type == FormQuestionType.CHOICE }
        return rows(input.type, r.allowCustom, r.allowAskBack, r.from.interaction, hasChoice)
    }
}
