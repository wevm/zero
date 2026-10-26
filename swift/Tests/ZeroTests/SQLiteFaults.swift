import CSQLite
import Foundation
import Synchronization

/// A SQLite VFS, made the default, that forwards to the system one and
/// fails writes or reads of the files whose path has `marker` on request.
enum SQLiteFaults {
  static let marker = "sweep-storage-fault-"
  /// The result code writes fail with, or 0.
  static let writeFault = Atomic<Int32>(0)
  /// How many more writes fail; negative for all.
  static let writeFaultsLeft = Atomic<Int>(0)
  /// The offset in a main database file whose reads fail, or -1.
  static let readFaultOffset = Atomic<Int64>(-1)
  /// How many reads failed.
  static let readFaultHits = Atomic<Int>(0)
  /// The offset in a main database file whose next read waits until
  /// `resumeRead()`, or -1. Only files opened while `pausePathFragment`
  /// was in their path are paused.
  static let pauseReadOffset = Atomic<Int64>(-1)
  /// Set while a read waits.
  static let readPaused = Atomic<Bool>(false)
  static let readResumed = Atomic<Bool>(false)
  nonisolated(unsafe) static var pausePathFragment: UnsafeMutablePointer<CChar>?

  static func failWrites(_ code: Int32, times: Int = -1) {
    writeFaultsLeft.store(times, ordering: .sequentiallyConsistent)
    writeFault.store(code, ordering: .sequentiallyConsistent)
  }

  static func failReads(atOffset offset: Int64) {
    readFaultOffset.store(offset, ordering: .sequentiallyConsistent)
  }

  static func reset() {
    writeFault.store(0, ordering: .sequentiallyConsistent)
    readFaultOffset.store(-1, ordering: .sequentiallyConsistent)
    readFaultHits.store(0, ordering: .sequentiallyConsistent)
  }

  /// The files of suites that run alongside the ones using `marker`, with
  /// fault state of their own, so neither arms faults on the other's files.
  static let isolatedMarker = "isolated-storage-fault-"
  /// The result code every write of a file with `isolatedMarker` in its
  /// path fails with, or 0.
  static let isolatedWriteFault = Atomic<Int32>(0)

  static func resetIsolated() {
    isolatedWriteFault.store(0, ordering: .sequentiallyConsistent)
    resumeRead()
  }

  /// Makes the next read at `offset` of a main database file opened from
  /// now on with `fragment` in its path wait until `resumeRead()`.
  static func pauseRead(ofFile fragment: String, atOffset offset: Int64) {
    readResumed.store(false, ordering: .sequentiallyConsistent)
    readPaused.store(false, ordering: .sequentiallyConsistent)
    pausePathFragment = strdup(fragment)
    pauseReadOffset.store(offset, ordering: .sequentiallyConsistent)
  }

  static func resumeRead() {
    pauseReadOffset.store(-1, ordering: .sequentiallyConsistent)
    readResumed.store(true, ordering: .sequentiallyConsistent)
  }

  /// Whether the next write of a marked file fails, and with what.
  static func takeWriteFault() -> Int32 {
    let code = writeFault.load(ordering: .sequentiallyConsistent)
    guard code != 0 else { return 0 }
    let left = writeFaultsLeft.load(ordering: .sequentiallyConsistent)
    if left == 0 { return 0 }
    if left > 0 {
      if writeFaultsLeft.subtract(1, ordering: .sequentiallyConsistent).newValue == 0 {
        writeFault.store(0, ordering: .sequentiallyConsistent)
      }
    }
    return code
  }

  // A wrapped file: our methods, a tag, then the system VFS's file.
  static let header = 16
  static let marked: Int64 = 1
  static let mainDB: Int64 = 2
  static let pauseTarget: Int64 = 4
  static let isolated: Int64 = 8

  nonisolated(unsafe) static var system: UnsafeMutablePointer<sqlite3_vfs>!
  nonisolated(unsafe) static var methods: UnsafeMutablePointer<sqlite3_io_methods>!

  static let installed: Bool = {
    system = sqlite3_vfs_find(nil)
    let vfs = UnsafeMutablePointer<sqlite3_vfs>.allocate(capacity: 1)
    vfs.initialize(to: system.pointee)
    vfs.pointee.pNext = nil
    vfs.pointee.zName = UnsafePointer(strdup("sweep-faults"))
    vfs.pointee.szOsFile = system.pointee.szOsFile + Int32(header)
    vfs.pointee.xOpen = { _, name, file, flags, outFlags in
      let inner = SQLiteFaults.inner(file!)
      let rc = SQLiteFaults.system.pointee.xOpen!(SQLiteFaults.system, name, inner, flags, outFlags)
      var tag: Int64 = 0
      if let name, strstr(name, SQLiteFaults.marker) != nil { tag |= SQLiteFaults.marked }
      if flags & SQLITE_OPEN_MAIN_DB != 0 { tag |= SQLiteFaults.mainDB }
      if let name, strstr(name, SQLiteFaults.isolatedMarker) != nil { tag |= SQLiteFaults.isolated }
      if let name, let fragment = SQLiteFaults.pausePathFragment, strstr(name, fragment) != nil {
        tag |= SQLiteFaults.pauseTarget
      }
      UnsafeMutableRawPointer(file!).advanced(by: 8).storeBytes(of: tag, as: Int64.self)
      file!.pointee.pMethods =
        inner.pointee.pMethods == nil ? nil : UnsafePointer(SQLiteFaults.methods)
      return rc
    }

    let m = UnsafeMutablePointer<sqlite3_io_methods>.allocate(capacity: 1)
    m.initialize(to: sqlite3_io_methods())
    // Version 2: WAL, without memory-mapped reads, so reads reach xRead.
    m.pointee.iVersion = 2
    m.pointee.xClose = { f in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xClose!(inner)
    }
    m.pointee.xRead = { f, buf, amt, off in
      let inner = SQLiteFaults.inner(f!)
      if SQLiteFaults.tag(f!) == SQLiteFaults.marked | SQLiteFaults.mainDB,
        off == SQLiteFaults.readFaultOffset.load(ordering: .sequentiallyConsistent)
      {
        SQLiteFaults.readFaultHits.add(1, ordering: .sequentiallyConsistent)
        return SQLITE_IOERR | (1 << 8)
      }
      if SQLiteFaults.tag(f!) & (SQLiteFaults.pauseTarget | SQLiteFaults.mainDB)
        == SQLiteFaults.pauseTarget | SQLiteFaults.mainDB,
        SQLiteFaults.pauseReadOffset.compareExchange(
          expected: off, desired: -1, ordering: .sequentiallyConsistent
        ).exchanged
      {
        SQLiteFaults.readPaused.store(true, ordering: .sequentiallyConsistent)
        while !SQLiteFaults.readResumed.load(ordering: .sequentiallyConsistent) { usleep(1000) }
      }
      return SQLiteFaults.real(inner).xRead!(inner, buf, amt, off)
    }
    m.pointee.xWrite = { f, buf, amt, off in
      let inner = SQLiteFaults.inner(f!)
      if SQLiteFaults.tag(f!) & SQLiteFaults.marked != 0 {
        let code = SQLiteFaults.takeWriteFault()
        if code != 0 { return code }
      }
      if SQLiteFaults.tag(f!) & SQLiteFaults.isolated != 0 {
        let code = SQLiteFaults.isolatedWriteFault.load(ordering: .sequentiallyConsistent)
        if code != 0 { return code }
      }
      return SQLiteFaults.real(inner).xWrite!(inner, buf, amt, off)
    }
    m.pointee.xTruncate = { f, size in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xTruncate!(inner, size)
    }
    m.pointee.xSync = { f, flags in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xSync!(inner, flags)
    }
    m.pointee.xFileSize = { f, size in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xFileSize!(inner, size)
    }
    m.pointee.xLock = { f, lock in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xLock!(inner, lock)
    }
    m.pointee.xUnlock = { f, lock in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xUnlock!(inner, lock)
    }
    m.pointee.xCheckReservedLock = { f, out in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xCheckReservedLock!(inner, out)
    }
    m.pointee.xFileControl = { f, op, arg in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xFileControl!(inner, op, arg)
    }
    m.pointee.xSectorSize = { f in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xSectorSize!(inner)
    }
    m.pointee.xDeviceCharacteristics = { f in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xDeviceCharacteristics!(inner)
    }
    m.pointee.xShmMap = { f, region, size, extend, out in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xShmMap!(inner, region, size, extend, out)
    }
    m.pointee.xShmLock = { f, offset, n, flags in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xShmLock!(inner, offset, n, flags)
    }
    m.pointee.xShmBarrier = { f in
      let inner = SQLiteFaults.inner(f!)
      SQLiteFaults.real(inner).xShmBarrier!(inner)
    }
    m.pointee.xShmUnmap = { f, delete in
      let inner = SQLiteFaults.inner(f!)
      return SQLiteFaults.real(inner).xShmUnmap!(inner, delete)
    }
    methods = m
    return sqlite3_vfs_register(vfs, 1) == SQLITE_OK
  }()

  static func inner(_ f: UnsafeMutablePointer<sqlite3_file>) -> UnsafeMutablePointer<sqlite3_file> {
    UnsafeMutableRawPointer(f).advanced(by: header).assumingMemoryBound(to: sqlite3_file.self)
  }

  static func tag(_ f: UnsafeMutablePointer<sqlite3_file>) -> Int64 {
    UnsafeMutableRawPointer(f).advanced(by: 8).load(as: Int64.self)
  }

  static func real(_ inner: UnsafeMutablePointer<sqlite3_file>) -> sqlite3_io_methods {
    inner.pointee.pMethods.pointee
  }
}
