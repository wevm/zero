/// The column `LiftField` writes the destination's field into. A space can't
/// start an identifier, so it never collides with a real column.
package let liftedFieldColumn = " lifted"

/// Lifts a field of a singular child relationship onto each row
/// (`packages/zql/src/ivm/lift-field.ts`).
///
/// For a junction aggregate the value (`label.points`) lives one hop past the
/// junction row (`issueLabel`). LiftField copies it onto the junction row, so
/// the `Aggregate` after it reduces a flat field. A change to the destination
/// arrives as a child change and becomes an edit of the lifted column; one
/// from below the destination leaves the value as it was. For a JSON path
/// (`jsonAggregate`) it lifts the leaf, as `aggregateValue` reads it.
///
/// The rows it emits carry no relationships.
final class LiftField: Operator {
  let input: any Input
  let sourceRelationship: String
  let field: String
  let jsonAggregate: AST.Aggregate?
  private let schema: SourceSchema
  weak var output: (any Output)?

  init(
    _ input: any Input, sourceRelationship: String, field: String,
    jsonAggregate: AST.Aggregate? = nil
  ) {
    self.input = input
    self.sourceRelationship = sourceRelationship
    self.field = field
    self.jsonAggregate = jsonAggregate
    let inputSchema = input.getSchema()
    var columns = inputSchema.columns
    // Nullable: a junction row may have no destination for a while.
    if let jsonAggregate {
      columns[liftedFieldColumn] = ColumnSchema(
        jsonAggregate.fn == .min || jsonAggregate.fn == .max ? .json : .number, optional: true)
    } else if var fieldSchema = inputSchema.relationship(sourceRelationship)?.columns[field] {
      fieldSchema.optional = true
      columns[liftedFieldColumn] = fieldSchema
    } else {
      columns[liftedFieldColumn] = ColumnSchema(.number, optional: true)
    }
    schema = SourceSchema(
      tableName: inputSchema.tableName, columns: columns, primaryKey: inputSchema.primaryKey,
      isHidden: inputSchema.isHidden, system: inputSchema.system,
      compareRows: inputSchema.compareRows, sort: inputSchema.sort)
    input.setOutput(self)
  }

  func setOutput(_ output: any Output) { self.output = output }
  func getSchema() -> SourceSchema { schema }
  func destroy() { input.destroy() }

  func fetch(_ req: FetchRequest) -> NodeStream {
    input.fetch(req).mapLazy { [self] in lift($0) }
  }

  func reconcile(pusher: any InputBase) { output?.reconcile(pusher: self) }

  func push(_ change: Change, pusher: any InputBase) {
    switch change {
    case .add(let node): output?.push(.add(lift(node)), pusher: self)
    case .remove(let node): output?.push(.remove(lift(node)), pusher: self)
    case .edit(let node, let oldNode):
      output?.push(.edit(lift(node), oldNode: lift(oldNode)), pusher: self)
    case .child(let node, let child):
      // A relationship this doesn't lift from, or a change below the
      // destination.
      guard child.relationshipName == sourceRelationship,
        let (oldValue, newValue) = values(child.change)
      else { return }
      var row = node.row
      row[liftedFieldColumn] = newValue
      var oldRow = node.row
      oldRow[liftedFieldColumn] = oldValue
      output?.push(.edit(Node(row: row), oldNode: Node(row: oldRow)), pusher: self)
    }
  }

  /// The value to lift from a destination row.
  private func read(_ row: Row) -> JSONValue {
    if let jsonAggregate { return aggregateValue(jsonAggregate, row) }
    return row[field] ?? .null
  }

  /// `node`'s row with the destination's field lifted onto it.
  private func lift(_ node: Node) -> Node {
    var value: JSONValue = .null
    if let rel = node.relationships[sourceRelationship] {
      for child in rel() { value = read(child.row) }
    }
    var row = node.row
    row[liftedFieldColumn] = value
    return Node(row: row)
  }

  /// The lifted value before and after a change to the destination; nil
  /// when the destination row itself didn't change.
  private func values(_ change: Change) -> (JSONValue, JSONValue)? {
    switch change {
    case .add(let node): (.null, read(node.row))
    case .remove(let node): (read(node.row), .null)
    case .edit(let node, let oldNode): (read(oldNode.row), read(node.row))
    case .child: nil
    }
  }
}
