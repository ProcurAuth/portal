package app.portalinbox.service

import android.content.Context
import app.portalinbox.core.NativeEffectHasher
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.ItemRepository
import app.portalinbox.data.SqlItemRepository
import app.portalinbox.data.Store

/** The one [ItemEngine] of the app: SQLite storage and the real effect hash (JNI). The screens read the same storage. */
object Items {
    @Volatile private var repo: SqlItemRepository? = null
    @Volatile private var engine: ItemEngine? = null

    fun repository(context: Context): ItemRepository = sql(context)

    private fun sql(context: Context): SqlItemRepository =
        repo ?: synchronized(this) { repo ?: SqlItemRepository(Store.get(context)).also { repo = it } }

    fun engine(context: Context): ItemEngine =
        engine ?: synchronized(this) {
            engine ?: ItemEngine(sql(context), NativeEffectHasher).also { engine = it }
        }
}
