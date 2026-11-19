---
'zero-swift': patch
---

Opening a client takes less of the main thread before the app's first frame: a third less for a replica of about 2,000 rows (on a Mac, 10.9 ms instead of 15.6 ms; on the iOS Simulator, 20.4 ms instead of 26.2 ms), and 40% less for 8,000 rows. The replica goes into the query sources without a diff against an empty one, and the first report of its queries and mutation results reads only those keys. The URLSession is made by the first connect, the network monitor starts on its own queue, and the storage folder is excluded from backups in the background.
