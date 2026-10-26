use desklink_session::DesktopRect;

use super::{CaptureError, DisplayDescriptor};

/// An immutable, canonical topology candidate. Build capture and encoding
/// resources first, then publish this value together with input coordinates.
/// Failed resource preparation must leave the previous snapshot active.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayTopologySnapshot {
    generation: u64,
    displays: Vec<DisplayDescriptor>,
    active_display_id: u32,
    virtual_desktop: DesktopRect,
}

impl DisplayTopologySnapshot {
    pub fn new(
        displays: Vec<DisplayDescriptor>,
        active_display_id: u32,
        virtual_desktop: DesktopRect,
    ) -> Result<Self, CaptureError> {
        let displays = canonical_displays(displays, virtual_desktop)?;
        if !displays
            .iter()
            .any(|display| display.id == active_display_id)
        {
            return Err(CaptureError::NoDisplay);
        }
        Ok(Self {
            generation: 1,
            displays,
            active_display_id,
            virtual_desktop,
        })
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn displays(&self) -> &[DisplayDescriptor] {
        &self.displays
    }

    pub const fn active_display_id(&self) -> u32 {
        self.active_display_id
    }

    pub fn active_display(&self) -> &DisplayDescriptor {
        self.displays
            .iter()
            .find(|display| display.id == self.active_display_id)
            .expect("snapshot retains its active display")
    }

    pub const fn virtual_desktop(&self) -> DesktopRect {
        self.virtual_desktop
    }

    /// Enumeration order alone is not a topology change. Monitor identity,
    /// full geometry, primary designation and virtual bounds are included.
    /// If the active output disappeared, explicitly choose primary/first.
    pub fn refreshed(
        &self,
        displays: Vec<DisplayDescriptor>,
        virtual_desktop: DesktopRect,
    ) -> Result<Option<Self>, CaptureError> {
        let displays = canonical_displays(displays, virtual_desktop)?;
        if displays == self.displays && virtual_desktop == self.virtual_desktop {
            return Ok(None);
        }
        let active_display_id = displays
            .iter()
            .find(|display| display.id == self.active_display_id)
            .or_else(|| displays.iter().find(|display| display.primary))
            .unwrap_or(&displays[0])
            .id;
        Ok(Some(Self {
            generation: self.next_generation()?,
            displays,
            active_display_id,
            virtual_desktop,
        }))
    }

    pub fn select(&self, display_id: u32) -> Result<Option<Self>, CaptureError> {
        if !self.displays.iter().any(|display| display.id == display_id) {
            return Err(CaptureError::NoDisplay);
        }
        if self.active_display_id == display_id {
            return Ok(None);
        }
        let mut selected = self.clone();
        selected.generation = self.next_generation()?;
        selected.active_display_id = display_id;
        Ok(Some(selected))
    }

    /// Resource recovery can change the capture/encoder generation while the
    /// monitor geometry stays the same. Advance the input wait boundary too.
    pub fn renewed(&self) -> Result<Self, CaptureError> {
        let mut renewed = self.clone();
        renewed.generation = self.next_generation()?;
        Ok(renewed)
    }

    /// A list-only update can retain capture/encoder resources, but an active
    /// output move (including negative origin) changes input geometry even
    /// when resolution stayed the same.
    pub fn requires_capture_reconfigure(&self, previous: &Self) -> bool {
        self.active_display_id != previous.active_display_id
            || self.active_display().rect != previous.active_display().rect
            || self.virtual_desktop != previous.virtual_desktop
    }

    fn next_generation(&self) -> Result<u64, CaptureError> {
        self.generation
            .checked_add(1)
            .ok_or_else(|| CaptureError::Native("display topology generation exhausted".into()))
    }
}

fn canonical_displays(
    mut displays: Vec<DisplayDescriptor>,
    virtual_desktop: DesktopRect,
) -> Result<Vec<DisplayDescriptor>, CaptureError> {
    if displays.is_empty() {
        return Err(CaptureError::NoDisplay);
    }
    if virtual_desktop.width == 0 || virtual_desktop.height == 0 {
        return Err(CaptureError::Native(
            "virtual display bounds are empty".into(),
        ));
    }
    displays.sort_unstable_by_key(|display| display.id);
    if displays.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(CaptureError::Native(
            "duplicate display identity in topology".into(),
        ));
    }
    if displays.iter().filter(|display| display.primary).count() > 1 {
        return Err(CaptureError::Native(
            "multiple primary displays in topology".into(),
        ));
    }
    let outer_right = i64::from(virtual_desktop.left) + i64::from(virtual_desktop.width);
    let outer_bottom = i64::from(virtual_desktop.top) + i64::from(virtual_desktop.height);
    for display in &displays {
        let rect = display.rect;
        if rect.width == 0
            || rect.height == 0
            || rect.left < virtual_desktop.left
            || rect.top < virtual_desktop.top
            || i64::from(rect.left) + i64::from(rect.width) > outer_right
            || i64::from(rect.top) + i64::from(rect.height) > outer_bottom
        {
            return Err(CaptureError::Native(
                "display geometry is outside virtual bounds".into(),
            ));
        }
    }
    Ok(displays)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u32, rect: DesktopRect, primary: bool) -> DisplayDescriptor {
        DisplayDescriptor {
            id,
            name: format!("display-{id}"),
            rect,
            primary,
        }
    }

    fn pair() -> (Vec<DisplayDescriptor>, DesktopRect) {
        (
            vec![
                display(7, DesktopRect::new(-1280, 0, 1280, 720), false),
                display(19, DesktopRect::new(0, 0, 1920, 1080), true),
            ],
            DesktopRect::new(-1280, 0, 3200, 1080),
        )
    }

    #[test]
    fn reordering_leaves_topology_generation_and_active_output_unchanged() {
        let (mut displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        displays.reverse();
        assert_eq!(current.generation(), 1);
        assert!(current.refreshed(displays, bounds).unwrap().is_none());
        assert_eq!(current.active_display_id(), 7);
    }

    #[test]
    fn active_removal_falls_back_to_primary_with_a_new_immutable_generation() {
        let (displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        let next = current
            .refreshed(vec![displays[1].clone()], displays[1].rect)
            .unwrap()
            .unwrap();
        assert_eq!(next.generation(), 2);
        assert_eq!(next.active_display_id(), 19);
        assert!(next.requires_capture_reconfigure(&current));
        assert_eq!(current.active_display_id(), 7);
        assert_eq!(current.displays().len(), 2);
    }

    #[test]
    fn rotation_origin_and_virtual_geometry_all_require_resource_coordination() {
        let (mut displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        displays[0].rect = DesktopRect::new(-720, 0, 720, 1280);
        let rotated_bounds = DesktopRect::new(-720, 0, 2640, 1280);
        let rotated = current
            .refreshed(displays.clone(), rotated_bounds)
            .unwrap()
            .unwrap();
        assert!(rotated.requires_capture_reconfigure(&current));
        displays[0].rect.left = -719;
        let moved = rotated
            .refreshed(displays, rotated_bounds)
            .unwrap()
            .unwrap();
        assert!(moved.requires_capture_reconfigure(&rotated));
        assert_eq!(moved.virtual_desktop(), rotated_bounds);
    }

    #[test]
    fn primary_or_name_only_change_publishes_metadata_without_capture_rebuild() {
        let (mut displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        displays[0].primary = true;
        displays[1].primary = false;
        displays[0].name = "renamed-source".into();
        let next = current.refreshed(displays, bounds).unwrap().unwrap();
        assert_eq!(next.generation(), 2);
        assert_eq!(next.active_display_id(), 7);
        assert!(!next.requires_capture_reconfigure(&current));
    }

    #[test]
    fn selecting_known_output_advances_generation_unknown_and_same_ids_do_not() {
        let (displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays, 7, bounds).unwrap();
        assert!(current.select(7).unwrap().is_none());
        assert!(current.select(99).is_err());
        let selected = current.select(19).unwrap().unwrap();
        assert_eq!(selected.generation(), 2);
        assert_eq!(selected.active_display_id(), 19);
        assert!(selected.requires_capture_reconfigure(&current));
        assert_eq!(current.generation(), 1);
    }

    #[test]
    fn invalid_or_temporarily_empty_enumeration_preserves_the_previous_snapshot() {
        let (displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        assert!(current.refreshed(vec![], bounds).is_err());
        assert!(
            current
                .refreshed(vec![displays[0].clone(), displays[0].clone()], bounds)
                .is_err()
        );
        assert!(
            current
                .refreshed(displays, DesktopRect::new(0, 0, 1920, 1080))
                .is_err()
        );
        assert_eq!(current.generation(), 1);
        assert_eq!(current.active_display_id(), 7);
    }

    #[test]
    fn no_primary_fallback_uses_stable_canonical_order_not_enumeration_order() {
        let (displays, bounds) = pair();
        let current = DisplayTopologySnapshot::new(displays.clone(), 7, bounds).unwrap();
        let next_displays = vec![
            display(30, DesktopRect::new(0, 0, 1920, 1080), false),
            display(22, DesktopRect::new(-1280, 0, 1280, 720), false),
        ];
        let next = current.refreshed(next_displays, bounds).unwrap().unwrap();
        assert_eq!(next.active_display_id(), 22);
    }
}
