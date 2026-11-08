package app.portalinbox.service

import android.content.Context
import app.portalinbox.core.NativeEffectHasher
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.SqlItemRepository
import app.portalinbox.data.Store

/** The one [ItemEngine] of the app: SQLite storage and the real effect hash (JNI). */
object Items {
    @Volatile private var engine: ItemEngine? = null

    fun engine(context: Context): ItemEngine =
        engine ?: synchronized(this) {
            engine ?: ItemEngine(SqlItemRepository(Store.get(context)), NativeEffectHasher).also { engine = it }
        }
}
