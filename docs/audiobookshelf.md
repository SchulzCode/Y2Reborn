# Audiobookshelf

Reborn can browse a book library on an Audiobookshelf server and download books for offline listening.
Connect the player to Wi-Fi, then open **Settings → Library → Audiobookshelf**.

Choose **Sign In** and enter the server's HTTPS base address, username and password using the player keyboard.
For a server hosted under a path, include that path in the base address, for example
`https://books.example.org/audiobookshelf`. Do not include a library page or its identifier. The server's
certificate must be trusted by the player. An account must be allowed to view the library and download its files.
OpenID is not required for local username and password sign-in.

Choose a book library, browse its pages or search, then open a book to see its cover and download it. Download
progress is shown on the book page. When a download finishes Reborn scans the library; the book then appears in
**Audiobooks** and can be played without a network connection. A finished book is marked **Downloaded**.

Reborn checks available internal storage before starting and keeps incomplete files out of the music library.
A retry starts the incomplete download again. Multipart M4B books need matching album tags to be recognised as a
single book. A download can fail if its file formats are unsupported, its metadata is incomplete, or storage is
unavailable or too full. Disconnect a computer from USB transfer before starting a download.

**Forget Account** removes the saved Audiobookshelf session from the player and asks the server to revoke it.
If the server cannot be reached, Reborn still removes the local session and says that the server session may remain.
Forgetting an account does not remove downloaded books.
Listening position is stored on the player; it is not synchronised back to Audiobookshelf.
