#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub started: Option<u64>,
    pub outputs: Vec<(u64, usize)>,
    pub inputs: Vec<(u64, String)>,
}

impl Timeline {
    pub fn parse(text: &str) -> Self {
        let mut timeline = Self::default();
        for line in text.lines() {
            let mut fields = line.splitn(3, ' ');
            let (Some(kind), Some(time)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Ok(time) = time.parse::<u64>() else {
                continue;
            };
            let rest = fields.next();
            match (kind, rest) {
                ("s", _) => timeline.started = Some(time),
                ("o", Some(end)) => {
                    if let Ok(end) = end.parse() {
                        timeline.outputs.push((time, end));
                    }
                }
                ("i", Some(label)) => timeline.inputs.push((time, label.to_owned())),
                _ => {}
            }
        }
        timeline
    }

    pub fn offset_at(&self, time: u64) -> usize {
        self.outputs
            .iter()
            .take_while(|(at, _)| *at <= time)
            .last()
            .map_or(0, |(_, end)| *end)
    }

    pub fn time_of_offset(&self, offset: usize) -> Option<u64> {
        self.outputs
            .iter()
            .find(|(_, end)| *end > offset)
            .map(|(at, _)| *at)
    }

    pub fn first_output_after(&self, time: u64) -> Option<u64> {
        self.outputs.iter().map(|(at, _)| *at).find(|at| *at > time)
    }

    pub fn last_output_before(&self, time: u64) -> Option<u64> {
        self.outputs
            .iter()
            .map(|(at, _)| *at)
            .take_while(|at| *at < time)
            .last()
    }

    pub fn inputs_labelled(&self, label: &str) -> Vec<u64> {
        self.inputs
            .iter()
            .filter(|(_, name)| name == label)
            .map(|(at, _)| *at)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str =
        "s 100\no 150 10\ni 200 scroll\no 210 30\ni 230 scroll\no 260 90\ni 400 quit\n";

    #[test]
    fn events_are_read_in_order() {
        let timeline = Timeline::parse(EVENTS);
        assert_eq!(timeline.started, Some(100));
        assert_eq!(timeline.outputs, vec![(150, 10), (210, 30), (260, 90)]);
        assert_eq!(timeline.inputs_labelled("scroll"), vec![200, 230]);
    }

    #[test]
    fn the_offset_at_a_time_is_what_had_been_written_by_then() {
        let timeline = Timeline::parse(EVENTS);
        assert_eq!(timeline.offset_at(99), 0);
        assert_eq!(timeline.offset_at(200), 10);
        assert_eq!(timeline.offset_at(500), 90);
    }

    #[test]
    fn a_byte_offset_maps_to_the_time_its_chunk_arrived() {
        let timeline = Timeline::parse(EVENTS);
        assert_eq!(timeline.time_of_offset(0), Some(150));
        assert_eq!(timeline.time_of_offset(10), Some(210));
        assert_eq!(timeline.time_of_offset(90), None);
    }

    #[test]
    fn responses_are_found_around_an_input() {
        let timeline = Timeline::parse(EVENTS);
        assert_eq!(timeline.first_output_after(200), Some(210));
        assert_eq!(timeline.last_output_before(400), Some(260));
        assert_eq!(timeline.first_output_after(300), None);
    }
}
