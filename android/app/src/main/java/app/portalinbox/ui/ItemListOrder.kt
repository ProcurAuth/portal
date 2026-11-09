package app.portalinbox.ui

import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.proto.RequestKind

/**
 * The order of the home list. Pure Kotlin.
 *
 * Alice's rule (checkpoint C3): the alerts come first. The section "To answer" comes below them. Inside "To answer",
 * the oldest request comes first: the one that waits longest is the first to read. [app.portalinbox.portal.ItemSort] sorts the rows.
 */
object ItemListOrder {
    enum class Section { ALERTS, TO_ANSWER }

    /** The sections that show, from the top. A section with no row does not show. Alerts are always above the items. */
    fun sections(hasAlerts: Boolean, hasItems: Boolean): List<Section> = buildList {
        if (hasAlerts) add(Section.ALERTS)
        if (hasItems) add(Section.TO_ANSWER)
    }

    // The order of the items across portals is in [app.portalinbox.portal.ItemSort]: it limits the time that a portal claims.

    /**
     * The section "To answer" as groups, one for each agent of each portal (lot G4). [sorted] are the open items that
     * [app.portalinbox.portal.ItemSort.toAnswer] sorted: the oldest first, across portals. The new agents come first, then the
     * groups by their oldest open item, the bucket last. Inside a group the items keep the order that they came in. The rules and
     * the model are in [AgentGroups].
     */
    fun toAnswer(sorted: List<Item>, directory: AgentDirectory, collapse: AgentCollapse): List<AgentGroups.Group> =
        AgentGroups.build(sorted, directory, collapse)

    enum class Chip { APPROVAL, QUESTION, DECISION }

    /** What a card of the list shows. */
    data class Row(
        val serverId: Long,
        val requestId: String,
        val chip: Chip,
        val title: String,
        /** "ops-agent · ClickHouse Dash". */
        val subtitle: String,
        /** The last line: ages, or the suggestion of an automatic system. */
        val detail: String,
        /** The left column: "6 d" and the code. */
        val age: String,
        val code: String?,
        val old: Boolean,
        val invalid: Boolean,
        /** The row opens a screen. A question opens its screen in lot A4. */
        val openable: Boolean,
    )

    /**
     * The card of a row. [label] is the name of the agent from the table of the agents ([AgentDirectory.labelOf]); the default is the
     * name that the frame carried. [grouped]: the row sits under the header of its agent, so the name of the agent does not repeat.
     */
    fun row(item: Item, nowMs: Long, label: String = item.request.from.label, grouped: Boolean = false): Row {
        val r = item.request
        val approval = r.approval
        val automatic = ItemTexts.isAutomatic(item)
        val chip = when {
            item.kind == RequestKind.APPROVAL -> Chip.APPROVAL
            automatic -> Chip.DECISION
            else -> Chip.QUESTION
        }
        val subtitle = when (chip) {
            Chip.APPROVAL -> listOfNotNull(label.takeUnless { grouped }, approval?.service).joinToString(" · ")
            Chip.DECISION -> listOfNotNull("Automatic system", label.takeUnless { grouped }).joinToString(" · ")
            Chip.QUESTION -> listOfNotNull(label.takeUnless { grouped }, ItemTexts.inputLine(item)).joinToString(" · ")
        }
        val detail = when {
            item.state == app.portalinbox.proto.ItemState.REPLIED -> "Waiting for the agent to answer"
            chip == Chip.APPROVAL -> ItemTexts.headerLine(item, nowMs)
            chip == Chip.DECISION -> r.question?.hint?.let { suggestion(it.suggested, it.confidence) }
                ?: ItemTexts.agentActive(item, nowMs).replaceFirstChar { it.uppercase() }
            else -> ItemTexts.agentActive(item, nowMs).replaceFirstChar { it.uppercase() }
        }
        return Row(
            serverId = item.serverId,
            requestId = item.requestId,
            chip = chip,
            title = ItemTexts.title(item),
            subtitle = VisibleText.escapeLine(subtitle),
            detail = detail,
            age = Ages.short(r.createdAtMs, nowMs),
            code = approval?.code,
            old = item.kind == RequestKind.APPROVAL && item.isOpen && ApprovalState.isStale(ApprovalState.ageOf(item, nowMs)),
            invalid = item.kind == RequestKind.APPROVAL && !item.validity.isValid,
            openable = item.kind == RequestKind.APPROVAL,
        )
    }

    private fun suggestion(suggested: String, confidence: Double?): String =
        "Suggestion: " + VisibleText.escapeLine(suggested) + (confidence?.let { " (" + Math.round(it * 100) + " %)" } ?: "")

    /** A card of the list "Done". */
    data class DoneRow(
        val serverId: Long,
        val requestId: String,
        val chip: Chip,
        val title: String,
        val word: String,
        val detail: String,
        val whenMs: Long,
        val openable: Boolean,
    )

    fun doneRow(item: Item): DoneRow = DoneRow(
        serverId = item.serverId,
        requestId = item.requestId,
        chip = if (item.kind == RequestKind.APPROVAL) Chip.APPROVAL else if (ItemTexts.isAutomatic(item)) Chip.DECISION else Chip.QUESTION,
        title = ItemTexts.title(item),
        word = ItemTexts.stateWord(item),
        detail = ItemTexts.reasonLine(item),
        whenMs = item.settled?.atMs ?: item.updatedAtMs,
        openable = item.kind == RequestKind.APPROVAL,
    )
}
