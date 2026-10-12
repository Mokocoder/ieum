use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[rustfmt::skip]
const EXPECTED: &[(&str, &str)] = &[
    ("첫걸음.이음", "안녕하세요, 이음입니다.\n7\n12\n1267650600228229401496703205376\n-2\n1\n0.25\n"),
    ("함수와반복.이음", "120\n15\n0\n1\n2\n1\n2\n"),
    ("함수인자.이음", "5\n7\n7\n[이음]\n"),
    ("조건과검색.이음", "양호\n배가 있습니다.\n[\"사과\", \"배\", \"감\", \"귤\"]\n[\"보고서\", \"csv\"]\n[\"나\"]\n하나 합격\n"),
    ("예외와정리.이음", "계산 실패: 0으로 나눌 수 없습니다.\n계산을 마쳤습니다.\n0\n계산을 마쳤습니다.\n5\n"),
    ("파일과경로.이음", "안녕하세요.\n이음에서 파일을 읽고 씁니다.\n인사.txt\n파일이 있습니다.\n"),
    (
        "가계부.이음",
        "[분류별 지출]
쇼핑  141,000원  69.1%
식비   51,700원  25.3%
카페   10,000원   4.9%
교통    1,450원   0.7%
합계  204,150원 (예산 초과)
[5만 원 넘는 지출]
2026-09-03 온라인몰 52,000원
2026-09-12 옷가게 89,000원
[읽지 못한 줄]
'잘못된금액'은 십진수로 읽을 수 없습니다.
",
    ),
    ("폴더정리.이음", "재고.csv: 1행\n점수.csv: 2행\n1\n"),
    (
        "재고관리.이음",
        "처리 실패 - 없는 품목입니다: 귤\n처리 실패 - 재고가 모자랍니다: 배\n부족한 품목:\n  사과: 3개\n  배: 4개\n",
    ),
    ("피즈버즈.이음", "1\n2\n피즈\n4\n버즈\n피즈\n7\n8\n피즈\n버즈\n11\n피즈\n13\n14\n피즈버즈\n16\n17\n피즈\n19\n버즈\n"),
    ("하노이탑.이음", "1번 원판을 왼쪽에서 오른쪽 기둥으로 옮깁니다.\n2번 원판을 왼쪽에서 가운데 기둥으로 옮깁니다.\n1번 원판을 오른쪽에서 가운데 기둥으로 옮깁니다.\n3번 원판을 왼쪽에서 오른쪽 기둥으로 옮깁니다.\n1번 원판을 가운데에서 왼쪽 기둥으로 옮깁니다.\n2번 원판을 가운데에서 오른쪽 기둥으로 옮깁니다.\n1번 원판을 왼쪽에서 오른쪽 기둥으로 옮깁니다.\n원판 3개를 7번 만에 옮겼습니다.\n"),
    ("에라토스테네스의체.이음", "100 이하의 소수 25개:\n  2  3  5  7 11 13 17 19 23 29\n 31 37 41 43 47 53 59 61 67 71\n 73 79 83 89 97\n"),
    ("여덟퀸.이음", "가로세로 8칸 판에 퀸 8개를 놓는 방법: 92가지\nQ . . . . . . .\n. . . . Q . . .\n. . . . . . . Q\n. . . . . Q . .\n. . Q . . . . .\n. . . . . . Q .\n. Q . . . . . .\n. . . Q . . . .\n"),
    ("생명게임.이음", "0세대\n. # . . . . . .\n. . # . . . . .\n# # # . . . . .\n. . . . . . . .\n. . . . . . . .\n. . . . . . . .\n1세대\n. . . . . . . .\n# . # . . . . .\n. # # . . . . .\n. # . . . . . .\n. . . . . . . .\n. . . . . . . .\n2세대\n. . . . . . . .\n. . # . . . . .\n# . # . . . . .\n. # # . . . . .\n. . . . . . . .\n. . . . . . . .\n3세대\n. . . . . . . .\n. # . . . . . .\n. . # # . . . .\n. # # . . . . .\n. . . . . . . .\n. . . . . . . .\n4세대\n. . . . . . . .\n. . # . . . . .\n. . . # . . . .\n. # # # . . . .\n. . . . . . . .\n. . . . . . . .\n"),
    ("함께기다리기.이음", "안녕\n세계\n안녕\n세계\n함께 기다리니 더 빨리 끝났습니다.\n"),
    ("병렬계산.이음", "0부터 25000개 중 소수 2762개\n25000부터 25000개 중 소수 2371개\n50000부터 25000개 중 소수 2260개\n75000부터 25000개 중 소수 2199개\n100000부터 25000개 중 소수 2142개\n125000부터 25000개 중 소수 2114개\n150000부터 25000개 중 소수 2068개\n175000부터 25000개 중 소수 2068개\n200000 미만의 소수는 모두 17984개입니다.\n"),
    (
        "숫자맞히기.이음",
        "1부터 20 사이의 수를 하나 골랐습니다. 여섯 번 안에 맞혀 보세요.\n어떤 수일까요? 제가 고른 수는 {수}입니다.\n",
    ),
    (
        "로그집계.이음",
        "INFO: 1줄\nWARN: 1줄\nERROR: 3줄\n깨진 줄: 1줄\n2회 결제 실패: 카드 거절\n1회 데이터베이스 연결 끊김\n",
    ),
    (
        "자료형.이음",
        "토지 (박경리)\n소년이 온다 (한강)\n책(제목: \"메모\", 저자: \"나\", 쪽수: 12)\n{\"제목\":\"토지\",\"저자\":\"박경리\",\"쪽수\":620}\n",
    ),
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn ieum(args: &[&str], cwd: &Path) -> String {
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(env!("CARGO_BIN_EXE_ieum"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        status.success(),
        "{args:?}\n{}",
        String::from_utf8_lossy(&stderr)
    );
    String::from_utf8(stdout).unwrap()
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_dir(self.0.parent().unwrap());
    }
}

fn sources(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|e| e == "이음") {
            found.push(path);
        }
    }
    found
}

#[test]
fn single_file_examples_print_their_documented_output() {
    let cwd = std::env::temp_dir().join(format!("ieum-examples-{}/작업", std::process::id()));
    let _cleanup = Cleanup(cwd.clone());
    fs::create_dir_all(&cwd).unwrap();
    let examples = root().join("examples");
    for path in sources(&examples) {
        if path.parent() != Some(examples.as_path()) {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap();
        let Some((_, expected)) = EXPECTED.iter().find(|(known, _)| *known == name) else {
            panic!("{name}: EXPECTED에 기대 출력을 추가하세요");
        };
        let output = ieum(&[path.to_str().unwrap()], &cwd);
        match expected.split_once("{수}") {
            Some((before, after)) => {
                let chosen = output
                    .strip_prefix(before)
                    .and_then(|rest| rest.strip_suffix(after))
                    .and_then(|number| number.parse::<u32>().ok());
                assert!(
                    chosen.is_some_and(|n| (1..=20).contains(&n)),
                    "{name}: {output}"
                );
            }
            None => assert_eq!(output, *expected, "{name}"),
        }
    }
    assert_eq!(
        fs::read_to_string(cwd.join("예제-출력/인사.txt")).unwrap(),
        "안녕하세요.\n이음에서 파일을 읽고 씁니다."
    );
}

#[test]
fn grade_report_example_runs_from_the_repository_root() {
    let folder = root().join("예제-출력/성적");
    let _cleanup = Cleanup(folder.clone());
    let stdout = ieum(&["examples/성적처리/시작.이음"], root());
    assert_eq!(stdout, "학생 수: 4\n결과 폴더: 예제-출력/성적\n");
    let report = fs::read_to_string(folder.join("결과.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["학생수"], 4);
    assert_eq!(report["합격수"], 3);
    assert_eq!(report["평균"], 81.25);
    assert_eq!(report["반별"]["가"]["총점"], 155);
    assert_eq!(
        fs::read_to_string(folder.join("결과.csv")).unwrap(),
        "이름,반,점수,결과\r\n\"김,하나\",가,95,합격\r\n이두나,나,80,합격\r\n박세나,가,60,불합격\r\n최네리,나,90,합격\r\n"
    );
}

#[test]
fn grade_report_tests_pass() {
    let stdout = ieum(&["test", "examples/성적처리"], root());
    assert!(stdout.contains("[통과] test_집계.이음"), "{stdout}");
}

#[test]
fn examples_are_formatted() {
    for path in sources(&root().join("examples")) {
        ieum(&["fmt", "--check", path.to_str().unwrap()], root());
    }
}
