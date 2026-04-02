#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TechnicalQueryCase {
    pub id: &'static str,
    pub question: &'static str,
    pub required_all: &'static [&'static str],
    pub required_any: &'static [&'static str],
    pub forbidden_any: &'static [&'static str],
}

pub fn exact_literal_cases() -> Vec<TechnicalQueryCase> {
    vec![
        TechnicalQueryCase {
            id: "checkout_server_system_info",
            question: "Какой endpoint возвращает текущую информацию checkout server?",
            required_all: &["/system/info"],
            required_any: &["GET", "system"],
            forbidden_any: &["/serverinfo"],
        },
        TechnicalQueryCase {
            id: "inventory_wsdl",
            question: "Какой WSDL у inventory soap api?",
            required_all: &["/inventory-api/ws/inventory.wsdl"],
            required_any: &["http://demo.local:8080"],
            forbidden_any: &["graphql", "/wsdl2"],
        },
    ]
}

pub fn unsupported_capability_cases() -> Vec<TechnicalQueryCase> {
    vec![TechnicalQueryCase {
        id: "graphql_absent",
        question: "Есть ли в этой библиотеке GraphQL API?",
        required_all: &[],
        required_any: &["нет", "not present", "не найден"],
        forbidden_any: &["/graphql", "graph ql endpoint"],
    }]
}

pub fn noisy_layout_cases() -> Vec<TechnicalQueryCase> {
    vec![
        TechnicalQueryCase {
            id: "page_number_param",
            question: "Как называется параметр pageNumber в API пагинации?",
            required_all: &["pageNumber"],
            required_any: &["page", "number"],
            forbidden_any: &["pageNu mber", "page_num ber"],
        },
        TechnicalQueryCase {
            id: "with_cards_param",
            question: "Есть ли параметр withCards?",
            required_all: &["withCards"],
            required_any: &["cards"],
            forbidden_any: &["withCar ds", "with_cards"],
        },
    ]
}

pub fn multihop_cases() -> Vec<TechnicalQueryCase> {
    vec![TechnicalQueryCase {
        id: "protocol_comparison",
        question: "Чем REST API rewards accounts отличается от inventory wsdl в транспортном контракте?",
        required_all: &[],
        required_any: &["REST", "WSDL", "SOAP", "HTTP"],
        forbidden_any: &["GraphQL"],
    }]
}
