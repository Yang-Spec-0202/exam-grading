//! 班级名单（`SPEC-align.md` §3.2、§5「名单为空/学号重复/姓名为空」）。
//!
//! 名单是身份匹配的唯一事实来源；加载期即拒绝不可用名单（整批显式失败），
//! 归一化键在构造时预计算，保证同一名单的查找结果稳定且可哈希。

use std::collections::BTreeMap;

use eg_domain::{ClassroomId, Sha256Hex, Student, StudentId};

use crate::error::AlignError;
use crate::normalize::normalize_key;

/// 名单（按班级）。
#[derive(Debug, Clone)]
pub struct Roster {
    classroom_id: ClassroomId,
    students: Vec<Student>,
    by_no: BTreeMap<String, StudentId>,
    by_name: BTreeMap<String, Vec<StudentId>>,
    by_id: BTreeMap<StudentId, Student>,
    hash: Sha256Hex,
}

impl Roster {
    /// 由学生列表构造并校验名单。
    ///
    /// # Errors
    ///
    /// 空名单、学号重复/为空、姓名为空、跨班级学生混入时返回 [`AlignError::Roster`]。
    pub fn new(classroom_id: ClassroomId, mut students: Vec<Student>) -> Result<Self, AlignError> {
        if students.is_empty() {
            return Err(AlignError::Roster(
                "名单为空（请先导入或维护名单）".to_owned(),
            ));
        }
        students.sort_by(|left, right| left.student_no.cmp(&right.student_no));
        let mut by_no: BTreeMap<String, StudentId> = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<StudentId>> = BTreeMap::new();
        let mut by_id: BTreeMap<StudentId, Student> = BTreeMap::new();
        for student in &students {
            if student.classroom_id != classroom_id {
                return Err(AlignError::Roster(format!(
                    "学生 {} 不属于班级 {classroom_id}",
                    student.id
                )));
            }
            if student.student_no.trim().is_empty() {
                return Err(AlignError::Roster(format!("学生 {} 学号为空", student.id)));
            }
            if student.name.trim().is_empty() {
                return Err(AlignError::Roster(format!("学生 {} 姓名为空", student.id)));
            }
            let key_no = normalize_key(&student.student_no);
            if key_no.is_empty() {
                return Err(AlignError::Roster(format!(
                    "学生 {} 学号归一后为空",
                    student.id
                )));
            }
            if by_no.insert(key_no, student.id).is_some() {
                return Err(AlignError::Roster(format!(
                    "学号重复：{}",
                    student.student_no
                )));
            }
            let key_name = normalize_key(&student.name);
            if key_name.is_empty() {
                return Err(AlignError::Roster(format!(
                    "学生 {} 姓名归一后为空",
                    student.id
                )));
            }
            by_name.entry(key_name).or_default().push(student.id);
            by_id.insert(student.id, student.clone());
        }
        let hash = roster_hash(&students);
        Ok(Self {
            classroom_id,
            students,
            by_no,
            by_name,
            by_id,
            hash,
        })
    }

    /// 班级 id。
    #[must_use]
    pub fn classroom_id(&self) -> ClassroomId {
        self.classroom_id
    }

    /// 名单人数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.students.len()
    }

    /// 是否为空（构造期已拒绝空名单，恒为 `false`；保留 API 完整性）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.students.is_empty()
    }

    /// 学生列表（按学号排序）。
    #[must_use]
    pub fn students(&self) -> &[Student] {
        &self.students
    }

    /// 名单哈希（进对齐指纹）。
    #[must_use]
    pub fn hash(&self) -> Sha256Hex {
        self.hash
    }

    /// 按学号查找（内部归一，名单内学号唯一）。
    #[must_use]
    pub fn by_student_no(&self, student_no: &str) -> Option<&Student> {
        let key = normalize_key(student_no);
        self.by_no.get(&key).and_then(|id| self.by_id.get(id))
    }

    /// 按姓名查找（内部归一；可能同名多人）。
    #[must_use]
    pub fn by_name(&self, name: &str) -> Vec<&Student> {
        let key = normalize_key(name);
        self.by_name
            .get(&key)
            .map(|ids| ids.iter().filter_map(|id| self.by_id.get(id)).collect())
            .unwrap_or_default()
    }

    /// 按 id 取学生。
    #[must_use]
    pub fn get(&self, id: StudentId) -> Option<&Student> {
        self.by_id.get(&id)
    }

    /// 学号后 4 位（日志/未决候选脱敏展示用）。
    #[must_use]
    pub fn student_no_tail(&self, id: StudentId) -> String {
        let Some(student) = self.by_id.get(&id) else {
            return String::new();
        };
        let digits: Vec<char> = student
            .student_no
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let tail: String = digits
            .iter()
            .rev()
            .take(4)
            .rev()
            .copied()
            .collect::<String>();
        if tail.is_empty() {
            format!("…{}", student.student_no)
        } else {
            format!("…{tail}")
        }
    }

    /// 姓名打码（保留首字，其余 `*`；未决候选展示用）。
    #[must_use]
    pub fn masked_name(&self, id: StudentId) -> String {
        let Some(student) = self.by_id.get(&id) else {
            return String::new();
        };
        mask_name(&student.name)
    }
}

/// 姓名打码：首字保留，其余打 `*`（单字姓名打 `*`）。
#[must_use]
pub fn mask_name(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let rest: String = chars.map(|_| '*').collect();
            if rest.is_empty() {
                "*".to_owned()
            } else {
                format!("{first}{rest}")
            }
        }
    }
}

fn roster_hash(students: &[Student]) -> Sha256Hex {
    let mut canonical = String::from("roster-1\n");
    for student in students {
        canonical.push_str(&student.id.to_string());
        canonical.push('\t');
        canonical.push_str(&normalize_key(&student.student_no));
        canonical.push('\t');
        canonical.push_str(&normalize_key(&student.name));
        canonical.push('\n');
    }
    Sha256Hex::of(canonical.as_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use eg_domain::{RosterSource, Ulid};

    use super::*;

    fn student(no: &str, name: &str) -> Student {
        Student {
            id: StudentId::from_ulid(Ulid::from_u128(
                u128::try_from(no.len()).unwrap_or(0) + u128::try_from(name.len()).unwrap_or(0),
            )),
            classroom_id: ClassroomId::from_ulid(Ulid::from_u128(1)),
            student_no: no.to_owned(),
            name: name.to_owned(),
            source: RosterSource::RosterImport,
        }
    }

    #[test]
    fn rejects_empty_and_duplicate_roster() {
        let classroom = ClassroomId::from_ulid(Ulid::from_u128(1));
        assert!(Roster::new(classroom, Vec::new()).is_err());
        let mut duplicate = student("01", "张三");
        duplicate.id = StudentId::from_ulid(Ulid::from_u128(9));
        assert!(Roster::new(classroom, vec![student("01", "张三"), duplicate]).is_err());
    }

    #[test]
    fn finds_by_normalized_no_and_name() {
        let classroom = ClassroomId::from_ulid(Ulid::from_u128(1));
        let roster = Roster::new(
            classroom,
            vec![student("20260001", "张三"), student("20260002", "李四")],
        )
        .unwrap();
        assert!(roster.by_student_no("20260001").is_some());
        assert!(roster.by_student_no("2026-0001").is_some());
        assert_eq!(roster.by_name("张三").len(), 1);
        assert_eq!(roster.by_name("王五").len(), 0);
        assert_eq!(roster.hash(), roster.hash());
    }

    #[test]
    fn masks_names_and_no_tails() {
        let classroom = ClassroomId::from_ulid(Ulid::from_u128(1));
        let roster = Roster::new(classroom, vec![student("20260001", "张三")]).unwrap();
        let id = roster.students()[0].id;
        assert_eq!(roster.student_no_tail(id), "…0001");
        assert_eq!(roster.masked_name(id), "张*");
        assert_eq!(mask_name("王"), "*");
    }
}
