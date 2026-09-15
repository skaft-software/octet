use sexy_tui_rs::rich_text::mermaid::render_mermaid;

#[test]
fn debug() {
    let cases = [
        "flowchart LR\n  A[Start] --> B[Done]",
        "graph TD\n  A[Start] --> B[Done]",
        "flowchart LR\n  A --> B",
        "flowchart LR\n  A[Foo]:::highlight --> B[Bar]",
        "flowchart LR\n  A[Parse] --> B[Layout] --> C[Render]",
        "flowchart LR\n  A[In] --> B{Valid?}\n  B --> C[Store]\n  B --> D[Reject]",
        "flowchart TD\n  A[In] --> B{Valid?}\n  B --> C[Store]\n  B --> D[Reject]",
        "flowchart LR\n  A -->|start| B\n  B --> C",
        "flowchart TD\n  A -->|start| B",
        "graph LR\n  A[Alpha] --> B[Beta]\n  A --> C[Gamma]\n  B --> D[Delta]\n  C --> D",
        "flowchart LR\n  A --> B --> C --> A",
        "flowchart LR\n  A --> C\n  A --> B\n  B --> C",
        "flowchart LR\n  A --> B & C",
        "pie\n  title Pets",
        "flowchart LR\n  A -- text --> B",
        "flowchart LR\n  A[One] --- B[Two]",
        "flowchart LR\n  A[One] -.-> B[Two]\n  A ==> C[Three]",
        "flowchart LR\n  subgraph S\n  A --> B\n  end",
    ];
    for source in cases {
        println!("--- {source:?}");
        match render_mermaid(source) {
            Ok(art) => {
                for line in &art.lines {
                    println!("|{line}");
                }
                println!("width={}", art.width);
            }
            Err(error) => println!("ERR {error}"),
        }
    }
}
