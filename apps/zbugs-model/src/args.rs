//! The arguments of zbugs' queries and mutators, as JSON in the shapes the
//! TS validators accept (`apps/zbugs/shared/queries.ts` and `mutators.ts`).
//!
//! A field the TS validator calls `nullable` is sent as `null` when `None`;
//! one it calls `optional` is left out. The drift test checks each against
//! what the TS side was given.

use crate::schema::IssueVisibility;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zero_client::json::Value;
use zero_client::typed::{Direction, ZeroValue};

/// `'asc' | 'desc'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn ast(self) -> Direction {
        match self {
            SortDirection::Asc => Direction::Asc,
            SortDirection::Desc => Direction::Desc,
        }
    }

    pub fn flipped(self) -> SortDirection {
        match self {
            SortDirection::Asc => SortDirection::Desc,
            SortDirection::Desc => SortDirection::Asc,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Modified,
    Created,
}

/// The filters and sort of an issue list (`listContextParams`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListContextParams {
    pub open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    pub assignee: Option<String>,
    pub creator: Option<String>,
    pub labels: Option<Vec<String>>,
    pub text_filter: Option<String>,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
}

/// Where an issue list starts: the issue's sort keys
/// (`issueRowSortSchema`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueRowSort {
    pub id: String,
    pub created: f64,
    pub modified: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageDirection {
    #[default]
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IdField {
    #[serde(rename = "shortID")]
    ShortId,
    #[serde(rename = "id")]
    Id,
}

impl IdField {
    pub fn column(self) -> &'static str {
        match self {
            IdField::ShortId => "shortID",
            IdField::Id => "id",
        }
    }
}

/// An issue's `id`, or its `shortID`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IssueKey {
    String(String),
    Number(f64),
}

impl IssueKey {
    pub fn json(&self) -> Value {
        match self {
            IssueKey::String(s) => Value::from(s.as_str()),
            IssueKey::Number(n) => Value::from(*n),
        }
    }

    /// An issue's id, or its short id when it's all digits
    /// (`getIDFromString` in `shared/issue-id.ts`).
    pub fn parse(s: &str) -> (IdField, IssueKey) {
        if s.bytes().any(|c| !c.is_ascii_digit()) {
            (IdField::Id, IssueKey::String(s.to_owned()))
        } else {
            (
                IdField::ShortId,
                IssueKey::Number(s.parse().unwrap_or(f64::NAN)),
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectArgs {
    pub project_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelsOrderByNameArgs {
    pub project_name: String,
    pub order_by: SortDirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsersFilter {
    Crew,
    Creators,
    Assignees,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsersForProjectArgs {
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<UsersFilter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommentCursor {
    pub id: String,
    pub created: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentsPageArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    pub limit: i64,
    pub start: Option<CommentCursor>,
    pub dir: PageDirection,
    pub inclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetailArgs {
    pub id_field: IdField,
    pub id: IssueKey,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListV2Args {
    pub list_context: ListContextParams,
    pub limit: Option<i64>,
    pub start: Option<IssueRowSort>,
    pub dir: PageDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusive: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListCountArgs {
    pub list_context: ListContextParams,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListIssueByIdArgs {
    pub list_context: ListContextParams,
    pub id_field: IdField,
    pub id_value: IssueKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrevNextDirection {
    Next,
    Prev,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrevNextArgs {
    pub list_context: Option<ListContextParams>,
    pub issue: Option<IssueRowSort>,
    pub dir: PrevNextDirection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListArgs {
    pub list_context: ListContextParams,
    pub limit: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserPickerFilter {
    Crew,
    Creators,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserPickerArgs {
    pub disabled: bool,
    pub login: Option<String>,
    pub filter: Option<UserPickerFilter>,
}

// Mutators.

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIssueArgs {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created: f64,
    pub modified: f64,
    #[serde(rename = "projectID")]
    pub project_id: String,
}

/// The fields of an issue to change; `None` leaves a field as it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateIssueArgs {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
    /// `Some(None)` unassigns the issue.
    #[serde(
        rename = "assigneeID",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub assignee_id: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "visibility::serialize",
        deserialize_with = "visibility::deserialize"
    )]
    pub visibility: Option<IssueVisibility>,
    pub modified: f64,
}

impl UpdateIssueArgs {
    /// A change of nothing but `modified`.
    pub fn new(id: impl Into<String>, modified: f64) -> Self {
        UpdateIssueArgs {
            id: id.into(),
            title: None,
            description: None,
            open: None,
            assignee_id: None,
            visibility: None,
            modified,
        }
    }
}

/// A field that is there, even as null: `Some(None)`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

mod visibility {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<IssueVisibility>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.serialize_str(v.as_str()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<IssueVisibility>, D::Error> {
        let s = String::deserialize(d)?;
        IssueVisibility::from_zero_value(&Value::from(s.as_str()))
            .map(Some)
            .map_err(|e| serde::de::Error::custom(e.0))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueLabelArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    #[serde(rename = "labelID")]
    pub label_id: String,
    /// Ignored: the issue's project is the label's. Still accepted for the
    /// clients that send it.
    #[serde(rename = "projectID", default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoveLabelArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    #[serde(rename = "labelID")]
    pub label_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationType {
    Subscribe,
    Unsubscribe,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotificationUpdateArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    pub subscribed: NotificationType,
    pub created: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddEmojiArgs {
    pub id: String,
    pub unicode: String,
    pub annotation: String,
    #[serde(rename = "subjectID")]
    pub subject_id: String,
    pub created: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddCommentArgs {
    pub id: String,
    #[serde(rename = "issueID")]
    pub issue_id: String,
    pub body: String,
    pub created: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditCommentArgs {
    pub id: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateLabelArgs {
    pub id: String,
    pub name: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateAndAddLabelArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    #[serde(rename = "labelID")]
    pub label_id: String,
    #[serde(rename = "labelName")]
    pub label_name: String,
    /// Ignored, as in [`IssueLabelArgs`].
    #[serde(rename = "projectID", default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewStateArgs {
    #[serde(rename = "issueID")]
    pub issue_id: String,
    pub viewed: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserPrefArgs {
    pub key: String,
    pub value: String,
}
