package app.portalinbox.ui

import android.content.Context
import android.content.Intent
import app.portalinbox.data.Item
import app.portalinbox.proto.RequestKind

/** Which screen opens an item: the approval screen for an approval, the question screen for a question (lot A4). */
object ItemScreens {
    enum class Screen { APPROVAL, QUESTION }

    fun of(kind: RequestKind): Screen = if (kind == RequestKind.APPROVAL) Screen.APPROVAL else Screen.QUESTION

    fun intent(context: Context, item: Item): Intent = intent(context, item.kind, item.serverId, item.requestId)

    fun intent(context: Context, kind: RequestKind, serverId: Long, requestId: String): Intent = when (of(kind)) {
        Screen.APPROVAL -> ApprovalActivity.intent(context, serverId, requestId)
        Screen.QUESTION -> QuestionActivity.intent(context, serverId, requestId)
    }
}
