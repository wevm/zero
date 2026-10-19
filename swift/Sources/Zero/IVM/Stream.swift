/// A lazy stream of values (`packages/zql/src/ivm/stream.ts`).
///
/// JS streams are generators, whose bodies start running on the first
/// `next()`. `IVMStream` matches that: `make` runs when the first element is
/// requested, not when the stream is created or iterated. Pipelines rely on
/// this, because a fetch reads operator and source state when it starts.
///
/// The JS `'yield'` tokens, which only hand control back to a scheduler,
/// have no counterpart: the Swift engine hydrates synchronously.
public struct IVMStream<Element>: Sequence {
  @usableFromInline let make: () -> AnyIterator<Element>

  /// A stream whose iterator `make` builds on the first `next()`.
  @inlinable
  public init(_ make: @escaping () -> AnyIterator<Element>) {
    self.make = make
  }

  /// A stream that yields `elements`.
  @inlinable
  public init(_ elements: [Element]) {
    self.make = { AnyIterator(elements.makeIterator()) }
  }

  @inlinable
  public static var empty: IVMStream<Element> {
    IVMStream { AnyIterator { nil } }
  }

  @inlinable
  public func makeIterator() -> LazyIterator<Element> {
    LazyIterator(make)
  }

  /// Maps each element; `f` runs as elements are pulled.
  @inlinable
  public func mapLazy<T>(_ f: @escaping (Element) -> T) -> IVMStream<T> {
    let source = self
    return IVMStream<T> {
      var it = source.makeIterator()
      return AnyIterator { it.next().map(f) }
    }
  }
}

/// Starts the underlying iterator on the first `next()`.
public struct LazyIterator<Element>: IteratorProtocol {
  @usableFromInline var make: (() -> AnyIterator<Element>)?
  @usableFromInline var iterator: AnyIterator<Element>?

  @inlinable
  init(_ make: @escaping () -> AnyIterator<Element>) {
    self.make = make
  }

  @inlinable
  public mutating func next() -> Element? {
    if let make {
      iterator = make()
      self.make = nil
    }
    return iterator?.next()
  }
}

/// Builds an iterator from a closure over local state; the closure runs
/// when the first element is requested.
@inlinable
func lazyStream<Element>(_ body: @escaping () -> () -> Element?) -> IVMStream<Element> {
  IVMStream { AnyIterator(body()) }
}

extension Sequence {
  /// The first element, pulling at most one.
  @inlinable
  var firstElement: Element? {
    var it = makeIterator()
    return it.next()
  }

  /// Pulls every element.
  @inlinable
  func consume() {
    var it = makeIterator()
    while it.next() != nil {}
  }
}

public typealias NodeStream = IVMStream<Node>
