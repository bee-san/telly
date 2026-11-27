package com.telly.player

import android.content.ContentResolver
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.net.Uri

/**
 * An Anki app on the phone that takes notes through AnkiDroid's provider
 * (its FlashCardsContract): AnkiDroid itself, or a fork with its own package,
 * provider and permission, like kuma3. AnkiDroid's API library has AnkiDroid's
 * names built in, so this asks the provider directly, under the app's own
 * authority.
 */
class AnkiApp private constructor(val name: String, val packageName: String, private val authority: String, val permission: String, val alias: String) {
  companion object {
    /** The apps Telly can add to, in the order it looks for them; each is in the manifest's <queries>. */
    private val KNOWN = listOf(
      AnkiApp("AnkiDroid", "com.ichi2.anki", "com.ichi2.anki.flashcards", "com.ichi2.anki.permission.READ_WRITE_DATABASE", "anki"),
      AnkiApp("kuma3", "io.github.pedrubik2000.kuma3", "io.github.pedrubik2000.kuma3.flashcards", "io.github.pedrubik2000.kuma3.permission.READ_WRITE_DATABASE", "kuma3"),
    )

    /** The first of them installed on this phone, if any. */
    fun find(context: Context): AnkiApp? = KNOWN.firstOrNull { context.packageManager.resolveContentProvider(it.authority, 0) != null }
  }

  private fun uri(vararg path: String): Uri = path.fold(Uri.parse("content://$authority")) { u, p -> Uri.withAppendedPath(u, p) }

  private fun <T> ContentResolver.each(uri: Uri, row: (Cursor) -> T): List<T>? =
    query(uri, null, null, null, null)?.use { c -> buildList { while (c.moveToNext()) add(row(c)) } }

  private fun Cursor.str(column: String) = getString(getColumnIndexOrThrow(column))

  /** Note types: id to name. */
  fun models(r: ContentResolver): Map<Long, String>? =
    r.each(uri("models")) { it.getLong(it.getColumnIndexOrThrow("_id")) to it.str("name") }?.toMap()

  /** Decks: id to name. */
  fun decks(r: ContentResolver): Map<Long, String>? =
    r.each(uri("decks")) { it.getLong(it.getColumnIndexOrThrow("deck_id")) to it.str("deck_name") }?.toMap()

  fun addDeck(r: ContentResolver, name: String): Long? =
    r.insert(uri("decks"), ContentValues().apply { put("deck_name", name) })?.lastPathSegment?.toLongOrNull()

  fun fieldNames(r: ContentResolver, model: Long): List<String>? =
    r.each(uri("models", model.toString())) { it.str("field_names") }?.firstOrNull()?.split('\u001f')

  /** Copies the file at `file` (readable by the app) into its media; returns what goes in a field. */
  fun addMedia(r: ContentResolver, file: Uri, name: String, kind: String): String? {
    val values = ContentValues().apply {
      put("file_uri", file.toString())
      put("preferred_name", name.replace(" ", "_"))
    }
    val saved = r.insert(uri("media"), values)?.path?.removePrefix("/") ?: return null
    return when (kind) {
      "audio" -> "[sound:$saved]"
      "image" -> "<img src=\"$saved\" />"
      else -> null
    }
  }

  /** Adds a note and moves its cards to `deck`; returns the note's id. */
  fun addNote(r: ContentResolver, model: Long, deck: Long, fields: List<String>, tags: Set<String>): Long? {
    val values = ContentValues().apply {
      put("mid", model)
      put("flds", fields.joinToString("\u001f"))
      put("tags", tags.joinToString(" ") { it.replace(" ", "_") })
    }
    // Only the id is taken from the URI the app gives back: an older kuma3 gave it under AnkiDroid's authority.
    val id = r.insert(uri("notes"), values)?.lastPathSegment ?: return null
    val ords = r.each(uri("notes", id, "cards")) { it.str("ord") } ?: return null
    for (ord in ords) r.update(uri("notes", id, "cards", ord), ContentValues().apply { put("deck_id", deck) }, null, null)
    return id.toLongOrNull()
  }
}
