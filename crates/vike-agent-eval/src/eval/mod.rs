//! What a case is and how it is judged: the prompt suite as data ([`cases`]), the deterministic grader ([`grade`]),
//! the end-to-end runner of one case ([`harness`]) and the verdict printer ([`report`]).

pub mod cases;
pub mod grade;
pub mod harness;
pub mod report;
